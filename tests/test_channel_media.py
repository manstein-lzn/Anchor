"""Real attachment bytes exercise parsers, bounds, and file authority boundaries."""
import base64
import hashlib
import io
import os
import zipfile

import pytest
from PIL import Image
from pypdf import PdfWriter
from pypdf.generic import DecodedStreamObject, DictionaryObject, NameObject

from anchor.channel import media


def attachment(tmp_path, name, data, **metadata):
    path = tmp_path / name
    path.write_bytes(data)
    return {'path': str(path), 'name': name, **metadata}


def image_bytes(fmt='PNG'):
    buffer = io.BytesIO()
    Image.new('RGB', (3, 2), (30, 80, 120)).save(buffer, format=fmt)
    return buffer.getvalue()


def office_bytes(files, compression=zipfile.ZIP_DEFLATED):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, 'w', compression) as archive:
        for name, data in files.items():
            archive.writestr(name, data)
    return buffer.getvalue()


def pdf_bytes(text='Real PDF evidence'):
    writer = PdfWriter()
    page = writer.add_blank_page(300, 300)
    font = DictionaryObject({NameObject('/Type'): NameObject('/Font'), NameObject('/Subtype'): NameObject('/Type1'),
                             NameObject('/BaseFont'): NameObject('/Helvetica')})
    page[NameObject('/Resources')] = DictionaryObject({NameObject('/Font'): DictionaryObject({NameObject('/F1'): font})})
    stream = DecodedStreamObject()
    stream.set_data(f'BT /F1 12 Tf 20 200 Td ({text}) Tj ET'.encode())
    page[NameObject('/Contents')] = writer._add_object(stream)
    buffer = io.BytesIO()
    writer.write(buffer)
    return buffer.getvalue()


@pytest.mark.parametrize('name', ['note.txt', 'note.md', 'note.csv', 'note.json'])
def test_utf8_content_is_read(tmp_path, name):
    text, images = media.prepare_attachments([attachment(tmp_path, name, '真实文件内容'.encode())])
    assert '真实文件内容' in text
    assert not images


@pytest.mark.parametrize(('fmt', 'mime'), [('PNG', 'image/png'), ('JPEG', 'image/jpeg'), ('WEBP', 'image/webp')])
def test_real_image_bytes_are_sniffed_not_metadata(tmp_path, fmt, mime):
    data = image_bytes(fmt)
    text, images = media.prepare_attachments([attachment(tmp_path, 'wrong.txt', data, mime_type='text/plain')])
    assert images == ((data, mime),)
    assert 'Validated image' in text


@pytest.mark.parametrize('fmt', ['PNG', 'JPEG'])
def test_msg_item_contains_original_bytes_and_digest(fmt):
    data = image_bytes(fmt)
    item = media.make_image_item(data)
    assert item['msgtype'] == 'image'
    assert base64.b64decode(item['image']['base64']) == data
    assert item['image']['md5'] == hashlib.md5(data, usedforsecurity=False).hexdigest()


def test_read_image_item_uses_safe_open_and_image_byte_limit(tmp_path, monkeypatch):
    data = image_bytes()
    attachment(tmp_path, 'image.png', data)
    path = tmp_path / 'image.png'
    assert media.read_image_item(path) == media.make_image_item(data)
    link = tmp_path / 'link.png'
    link.symlink_to(path)
    parent = tmp_path / 'linked-parent'
    parent.symlink_to(tmp_path, target_is_directory=True)
    for unsafe in [link, parent / 'image.png']:
        with pytest.raises(OSError):
            media.read_image_item(unsafe)
    monkeypatch.setattr(media, 'MAX_IMAGE_BYTES', len(data) - 1)
    with pytest.raises(ValueError, match='byte limit'):
        media.read_image_item(path)


def test_failed_parser_still_consumes_aggregate_byte_budget(tmp_path, monkeypatch):
    monkeypatch.setattr(media, 'MAX_TOTAL_BYTES', 5)
    text, images = media.prepare_attachments([
        attachment(tmp_path, 'bad.png', b'wrong'),
        attachment(tmp_path, 'good.txt', b'OK'),
    ])
    assert text.count('Not read') == 2
    assert 'byte limit' in text
    assert 'OK' not in text
    assert not images


@pytest.mark.parametrize('data', [b'not an image', b'\x89PNG\r\n\x1a\n', b'\xff\xd8\xff', b''])
def test_corrupt_images_are_not_sent_or_claimed_read(tmp_path, data):
    text, images = media.prepare_attachments([attachment(tmp_path, 'corrupt.png', data, kind='image')])
    assert not images
    assert 'Not read' in text
    with pytest.raises(ValueError):
        media.make_image_item(data)


def test_image_byte_pixel_and_aggregate_limits(tmp_path, monkeypatch):
    data = image_bytes()
    monkeypatch.setattr(media, 'MAX_IMAGE_BYTES', len(data) - 1)
    with pytest.raises(ValueError, match='10 MiB'):
        media.make_image_item(data)
    monkeypatch.setattr(media, 'MAX_IMAGE_BYTES', len(data))
    monkeypatch.setattr(media, 'MAX_IMAGE_PIXELS', 5)
    with pytest.raises(ValueError, match='pixel'):
        media.make_image_item(data)
    monkeypatch.setattr(media, 'MAX_IMAGE_PIXELS', 6)
    monkeypatch.setattr(media, 'MAX_IMAGE_TOTAL_BYTES', len(data))
    text, images = media.prepare_attachments([attachment(tmp_path, 'one.png', data), attachment(tmp_path, 'two.png', data)])
    assert len(images) == 1
    assert 'aggregate image limit' in text


def test_animated_and_unsupported_output_images_rejected(tmp_path):
    with pytest.raises(ValueError, match='unsupported image format'):
        media.make_image_item(image_bytes('WEBP'))
    buffer = io.BytesIO()
    frames = [Image.new('RGB', (2, 2), color) for color in ('red', 'blue')]
    frames[0].save(buffer, 'PNG', save_all=True, append_images=frames[1:])
    with pytest.raises(ValueError, match='animated'):
        media.make_image_item(buffer.getvalue())
    with pytest.raises(ValueError, match='bytes'):
        media.make_image_item('/host/path.png')


def test_pdf_extraction_contains_real_text_and_page_locator(tmp_path):
    text, images = media.prepare_attachments([attachment(tmp_path, 'report.pdf', pdf_bytes())])
    assert 'Real PDF evidence' in text
    assert '[Page 1]' in text
    assert 'scanned pages and figures are not visually read' in text
    assert not images


def test_bad_and_encrypted_pdf_explicitly_not_read(tmp_path):
    writer = PdfWriter()
    writer.add_blank_page(300, 300)
    writer.encrypt('private')
    buffer = io.BytesIO()
    writer.write(buffer)
    for name, data in [('bad.pdf', b'%PDF-invalid'), ('encrypted.pdf', buffer.getvalue())]:
        text, images = media.prepare_attachments([attachment(tmp_path, name, data)])
        assert 'Not read' in text
        assert not images


def test_office_content_is_read_without_extracting_archive(tmp_path):
    docx = office_bytes({'word/document.xml': '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Document evidence</w:t></w:r></w:p></w:body></w:document>'})
    xlsx = office_bytes({
        'xl/sharedStrings.xml': '<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>Measured voltage</t></si></sst>',
        'xl/worksheets/sheet1.xml': '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row><c r="A1" t="s"><v>0</v></c><c r="B1"><v>3.3</v></c><c r="C1" t="inlineStr"><is><t>V</t></is></c></row></sheetData></worksheet>',
    })
    text, images = media.prepare_attachments([attachment(tmp_path, 'report.docx', docx), attachment(tmp_path, 'data.xlsx', xlsx)])
    assert 'Document evidence' in text
    assert 'A1: Measured voltage\tB1: 3.3\tC1: V' in text
    assert not images
    assert sorted(p.name for p in tmp_path.iterdir()) == ['data.xlsx', 'report.docx']


def test_office_zip_bomb_xml_entities_and_corruption_are_rejected(tmp_path):
    bad = [
        b'corrupt archive',
        office_bytes({'word/document.xml': 'a' * 1_000_000}),
        office_bytes({'word/document.xml': '<!DOCTYPE foo [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><foo>&xxe;</foo>'}),
    ]
    for index, data in enumerate(bad):
        text, images = media.prepare_attachments([attachment(tmp_path, f'bad{index}.docx', data)])
        assert 'Not read' in text
        assert not images


def test_office_total_expansion_and_member_size_bounds(tmp_path, monkeypatch):
    data = office_bytes({'word/document.xml': '<document>content</document>'}, zipfile.ZIP_STORED)
    item = attachment(tmp_path, 'report.docx', data)
    monkeypatch.setattr(media, 'MAX_ZIP_BYTES', 5)
    assert 'expansion limit' in media.prepare_attachments([item])[0]
    monkeypatch.setattr(media, 'MAX_ZIP_BYTES', 100)
    monkeypatch.setattr(media, 'MAX_XML_BYTES', 5)
    assert 'XML size limit' in media.prepare_attachments([item])[0]


def test_symlinks_parents_fifo_and_directory_are_not_followed(tmp_path):
    item = attachment(tmp_path, 'original.txt', b'secret')
    link = tmp_path / 'linked.txt'
    link.symlink_to(item['path'])
    parent = tmp_path / 'linked-directory'
    parent.symlink_to(tmp_path, target_is_directory=True)
    fifo = tmp_path / 'pipe.txt'
    os.mkfifo(fifo)
    for path in [link, parent / 'original.txt', fifo, tmp_path]:
        text, images = media.prepare_attachments([{'path': str(path)}])
        assert 'Not read' in text
        assert 'secret' not in text
        assert not images


def test_byte_limits_and_text_truncation_are_explicit(tmp_path, monkeypatch):
    item = attachment(tmp_path, 'too-big.txt', b'x' * 12)
    monkeypatch.setattr(media, 'MAX_FILE_BYTES', 10)
    assert 'byte limit' in media.prepare_attachments([item])[0]
    monkeypatch.setattr(media, 'MAX_FILE_BYTES', 20)
    monkeypatch.setattr(media, 'MAX_TOTAL_BYTES', 12)
    assert 'byte limit' in media.prepare_attachments([item, item])[0]
    monkeypatch.setattr(media, 'MAX_TOTAL_BYTES', 100_000)
    monkeypatch.setattr(media, 'MAX_FILE_BYTES', 100_000)
    long_item = attachment(tmp_path, 'long.txt', b'z' * (media.MAX_FILE_TEXT_CHARS + 1))
    text, images = media.prepare_attachments([long_item] * 5)
    assert 'truncated' in text
    assert len(text) <= media.MAX_TEXT_CHARS
    assert not images
    with pytest.raises(ValueError, match='16'):
        media.prepare_attachments([item] * 17)


def test_unsupported_archive_and_binary_not_claimed_read(tmp_path):
    for name, data in [('raw.zip', office_bytes({'a.txt': 'secret'})), ('file.bin', b'opaque'), ('wrong.txt', b'a\x00b')]:
        text, images = media.prepare_attachments([attachment(tmp_path, name, data)])
        assert 'Not read' in text
        assert not images
    assert media.prepare_attachments([]) == ('', ())
