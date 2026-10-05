"""Bounded, read-only attachment preparation; no model or platform clients.

Paths must already be authorized by the caller against its trusted input root.
These helpers reject symlinks but do not confer authority to read host paths.
"""
from __future__ import annotations

import base64
import hashlib
import io
import os
from pathlib import Path
import stat
import subprocess
import sys
import warnings
import zipfile

MAX_ATTACHMENTS = 16
MAX_FILE_BYTES = 20 * 1024 * 1024
MAX_TOTAL_BYTES = 50 * 1024 * 1024
MAX_IMAGE_BYTES = 10 * 1024 * 1024
MAX_IMAGE_TOTAL_BYTES = 20 * 1024 * 1024
MAX_IMAGES = 8
MAX_IMAGE_PIXELS = 20_000_000
MAX_TEXT_CHARS = 48_000
MAX_FILE_TEXT_CHARS = 12_000
MAX_ZIP_BYTES = 32 * 1024 * 1024
MAX_XML_BYTES = 8 * 1024 * 1024
MAX_ZIP_ENTRIES = 2048
MAX_PDF_PAGES = 100
_TRUNCATED = '\n[Content truncated by attachment limit]'
_TEXT_SUFFIXES = {'.txt', '.md', '.csv', '.json', '.log', '.tsv'}
_IMAGE_SUFFIXES = {'.png', '.jpg', '.jpeg', '.webp', '.gif', '.bmp', '.tif', '.tiff'}


def _clip(text: str, limit: int) -> str:
    return text if len(text) <= limit else text[:max(0, limit - len(_TRUNCATED))] + _TRUNCATED[:limit]


def _read_file(path: Path, remaining: int) -> bytes:
    """Use directory descriptors so neither parents nor the final file follow links."""
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('an absolute authorized path without .. is required')
    directory = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        with os.fdopen(fd, 'rb') as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode):
                raise ValueError('only regular files can be read')
            limit = min(MAX_FILE_BYTES, remaining)
            if info.st_size > limit:
                raise ValueError('file or aggregate attachment byte limit exceeded')
            data = stream.read(limit + 1)
            if len(data) > limit:
                raise ValueError('file or aggregate attachment byte limit exceeded')
            return data
    finally:
        os.close(directory)


def _image_mime(data: bytes, *, output: bool = False) -> str:
    from PIL import Image, UnidentifiedImageError

    if not data or len(data) > MAX_IMAGE_BYTES:
        raise ValueError('image must be nonempty and at most 10 MiB')
    allowed = {'PNG': 'image/png', 'JPEG': 'image/jpeg'}
    if not output:
        allowed['WEBP'] = 'image/webp'
    try:
        with warnings.catch_warnings():
            warnings.simplefilter('error', Image.DecompressionBombWarning)
            with Image.open(io.BytesIO(data)) as im:
                mime = allowed.get(im.format or '')
                if mime is None:
                    raise ValueError('unsupported image format; PNG/JPEG' + ('' if output else '/WebP') + ' required')
                if im.width * im.height > MAX_IMAGE_PIXELS:
                    raise ValueError('image pixel limit exceeded')
                if getattr(im, 'n_frames', 1) != 1:
                    raise ValueError('animated images are unsupported')
                im.verify()
            # verify() alone does not decode JPEG pixel data or detect every truncated image.
            with Image.open(io.BytesIO(data)) as im:
                im.load()
        return mime
    except (UnidentifiedImageError, OSError, SyntaxError, Image.DecompressionBombError,
            Image.DecompressionBombWarning) as exc:
        raise ValueError('invalid or unsafe image') from exc


def make_image_item(data: bytes) -> dict:
    """Build one internal image item from validated PNG/JPEG bytes, never a path/URL."""
    if not isinstance(data, bytes):
        raise ValueError('image data must be bytes')
    _image_mime(data, output=True)
    return {'msgtype': 'image', 'image': {
        'base64': base64.b64encode(data).decode('ascii'),
        'md5': hashlib.md5(data, usedforsecurity=False).hexdigest(),
    }}


def read_image_item(path: Path) -> dict:
    """Read a caller-authorized image without following any path-component links."""
    return make_image_item(_read_file(path, MAX_IMAGE_BYTES))


def _xml(archive: zipfile.ZipFile, name: str):
    from defusedxml.ElementTree import fromstring

    info = archive.getinfo(name)
    if info.file_size > MAX_XML_BYTES:
        raise ValueError('Office XML size limit exceeded')
    with archive.open(info) as stream:
        data = stream.read(MAX_XML_BYTES + 1)
    if len(data) > MAX_XML_BYTES:
        raise ValueError('Office XML size limit exceeded')
    return fromstring(data, forbid_dtd=True)


def _office_text(data: bytes, suffix: str) -> str:
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        if len(entries) > MAX_ZIP_ENTRIES or sum(i.file_size for i in entries) > MAX_ZIP_BYTES:
            raise ValueError('Office archive expansion limit exceeded')
        if len({i.filename for i in entries}) != len(entries):
            raise ValueError('duplicate Office archive members')
        for info in entries:
            if info.flag_bits & 1 or info.file_size > max(1, info.compress_size) * 200:
                raise ValueError('encrypted or excessive-compression Office archive')
        if suffix == '.docx':
            root = _xml(archive, 'word/document.xml')
            ns = '{http://schemas.openxmlformats.org/wordprocessingml/2006/main}'
            lines = [''.join(node.itertext()) for node in root.iter(ns + 'p')]
            text = '\n'.join(lines)
            note = '[DOCX main-body text only; drawings, headers, footers and embedded files are not read]\n'
        else:
            text = _xlsx_text(archive)
            note = '[XLSX cell values; formulas use stored values, drawings and embedded files are not read]\n'
        return note + _clip(text, MAX_FILE_TEXT_CHARS - len(note))


def _xlsx_text(archive: zipfile.ZipFile) -> str:
    ns = '{http://schemas.openxmlformats.org/spreadsheetml/2006/main}'
    names = archive.namelist()
    shared = []
    if 'xl/sharedStrings.xml' in names:
        shared = [''.join(node.itertext()) for node in _xml(archive, 'xl/sharedStrings.xml').iter(ns + 'si')]
    sheets = sorted(n for n in names if n.startswith('xl/worksheets/sheet') and n.endswith('.xml'))
    if not sheets:
        raise ValueError('no worksheets found')
    lines: list[str] = []
    length = 0
    for name in sheets:
        lines.append('[' + name + ']')
        for row in _xml(archive, name).iter(ns + 'row'):
            cells = []
            for cell in row.iter(ns + 'c'):
                value = cell.findtext(ns + 'v', '')
                if cell.get('t') == 's':
                    index = int(value)
                    if index < 0 or index >= len(shared):
                        raise ValueError('invalid shared string index')
                    value = shared[index]
                elif cell.get('t') == 'inlineStr':
                    value = ''.join(t.text or '' for t in cell.iter(ns + 't'))
                cells.append(f'{cell.get("r", "?")}: {value}')
            line = '\t'.join(cells)
            lines.append(line)
            length += len(line) + 1
            if length > MAX_FILE_TEXT_CHARS:
                return _clip('\n'.join(lines), MAX_FILE_TEXT_CHARS)
    return '\n'.join(lines)


def _pdf_text(data: bytes) -> str:
    try:
        result = subprocess.run(
            [sys.executable, str(Path(__file__).absolute()), '--pdf'], input=data,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=12, check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise ValueError('PDF extraction time limit exceeded') from exc
    if result.returncode:
        raise ValueError('PDF unreadable, encrypted, or extraction resource limit exceeded')
    return result.stdout.decode('utf-8')


def _extract(data: bytes, item: dict) -> tuple[str, str | None]:
    suffix = Path(str(item.get('name') or item['path'])).suffix.lower()
    mime = str(item.get('mime_type', '')).lower()
    image_signature = (data.startswith((b'\x89PNG', b'\xff\xd8\xff', b'GIF8'))
                       or data[:4] == b'RIFF' and data[8:12] == b'WEBP')
    if image_signature or item.get('kind') == 'image' or suffix in _IMAGE_SUFFIXES or mime.startswith('image/'):
        return '[Validated image supplied separately to the model]', _image_mime(data)
    if data.startswith(b'%PDF-') or suffix == '.pdf' or mime == 'application/pdf':
        return _pdf_text(data), None
    if suffix in {'.docx', '.xlsx'}:
        return _office_text(data, suffix), None
    if data.startswith(b'PK\x03\x04'):
        return '[Not read: unsupported archive]', None
    if suffix in _TEXT_SUFFIXES or mime.startswith('text/') or mime == 'application/json':
        text = data.decode('utf-8-sig')
        if '\x00' in text:
            raise ValueError('binary data is not UTF-8 text')
        return _clip(text, MAX_FILE_TEXT_CHARS), None
    return '[Not read: unsupported attachment type; original remains available in read-only input]', None


def prepare_attachments(items: list[dict], *, contents: tuple[bytes, ...] | None = None
                        ) -> tuple[str, tuple[tuple[bytes, str], ...]]:
    """Return bounded extracted text and (image bytes, sniffed MIME) tuples.

    Bad individual files produce explicit ``Not read`` notices and no image.
    More than 16 items is a request error. The caller still authorizes paths.
    A trusted caller may supply bytes read once from those paths, so extraction
    and upload use identical content even if the original file later changes.
    """
    if len(items) > MAX_ATTACHMENTS:
        raise ValueError('at most 16 attachments are allowed')
    if contents is not None and (len(contents) != len(items) or
                                 any(not isinstance(data, bytes) for data in contents)):
        raise ValueError('attachment contents must match the supplied files')
    from defusedxml.common import DefusedXmlException
    from xml.etree.ElementTree import ParseError

    parts: list[str] = []
    images: list[tuple[bytes, str]] = []
    total = image_total = 0
    for index, item in enumerate(items):
        name = str(item.get('name') or Path(str(item.get('path', 'attachment'))).name)[:200]
        name = name.replace('\n', ' ').replace('\r', ' ')
        try:
            data = contents[index] if contents is not None else _read_file(Path(item['path']), MAX_TOTAL_BYTES - total)
            if len(data) > min(MAX_FILE_BYTES, MAX_TOTAL_BYTES - total):
                raise ValueError('file or aggregate attachment byte limit exceeded')
            total += len(data)
            content, mime = _extract(data, item)
            if mime:
                if len(images) >= MAX_IMAGES or image_total + len(data) > MAX_IMAGE_TOTAL_BYTES:
                    raise ValueError('aggregate image limit exceeded')
                images.append((data, mime))
                image_total += len(data)
        except (OSError, ValueError, KeyError, TypeError, zipfile.BadZipFile, NotImplementedError,
                DefusedXmlException, ParseError) as exc:
            # Do not expose host paths, parser internals, or arbitrary exception content.
            reason = str(exc) if isinstance(exc, ValueError) and not isinstance(exc, UnicodeError) else type(exc).__name__
            content = f'[Not read: {reason[:180]}]'
        parts.append(f'Attachment: {name}\n{content}')
    return _clip('\n\n'.join(parts), MAX_TEXT_CHARS), tuple(images)


def _pdf_worker() -> None:
    import resource

    resource.setrlimit(resource.RLIMIT_AS, (384 * 1024 * 1024, 384 * 1024 * 1024))
    resource.setrlimit(resource.RLIMIT_CPU, (8, 8))
    from pypdf import PdfReader

    reader = PdfReader(io.BytesIO(sys.stdin.buffer.read(MAX_FILE_BYTES + 1)), strict=True)
    if reader.is_encrypted:
        raise ValueError('encrypted PDF')
    parts = ['[PDF text extraction only; scanned pages and figures are not visually read]']
    for number, page in enumerate(reader.pages):
        if number >= MAX_PDF_PAGES:
            parts.append(_TRUNCATED)
            break
        parts.append(f'[Page {number + 1}]\n' + (page.extract_text() or '[No extractable text]'))
        if sum(map(len, parts)) > MAX_FILE_TEXT_CHARS:
            break
    sys.stdout.buffer.write(_clip('\n'.join(parts), MAX_FILE_TEXT_CHARS).encode('utf-8'))


if __name__ == '__main__' and sys.argv[1:] == ['--pdf']:
    _pdf_worker()
