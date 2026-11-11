//! Bounded file bytes shared by private channel control adapters.
use crate::GatewayError;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::Md5;

pub const MAX_OUTBOUND_FILE_BYTES: usize = 20 * 1024 * 1024;

pub struct OutboundFile {
    pub name: String,
    pub bytes: Vec<u8>,
    pub md5: String,
}

/// A proactive file is handed over as bytes, never as an arbitrary Host path.
pub fn outbound_file(
    name: String,
    encoded: &str,
    declared_md5: &str,
) -> Result<OutboundFile, GatewayError> {
    if name.trim().is_empty()
        || name.len() > 256
        || name
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
        || matches!(name.as_str(), "." | "..")
    {
        return Err(GatewayError::Invalid("invalid outbound filename"));
    }
    if encoded.len() > MAX_OUTBOUND_FILE_BYTES.div_ceil(3) * 4 {
        return Err(GatewayError::Invalid(
            "outbound file exceeds the size limit",
        ));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| GatewayError::Invalid("outbound file is not valid base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_OUTBOUND_FILE_BYTES {
        return Err(GatewayError::Invalid(
            "outbound file size is outside the supported range",
        ));
    }
    let md5 = format!("{:x}", <Md5 as md5::Digest>::digest(&bytes));
    if md5 != declared_md5 {
        return Err(GatewayError::Invalid(
            "outbound file digest does not match its content",
        ));
    }
    Ok(OutboundFile { name, bytes, md5 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbound_files_validate_safe_names_bounded_nonempty_bytes_and_digest() {
        let encoded = STANDARD.encode(b"x");
        let digest = format!("{:x}", <Md5 as md5::Digest>::digest(b"x"));
        let file = outbound_file("one.txt".into(), &encoded, &digest).unwrap();
        assert_eq!(file.bytes, b"x");
        assert_eq!(file.md5, digest);
        for name in ["", " ", ".", "..", "../file", "dir\\file", "file\n"] {
            assert!(outbound_file(name.into(), &encoded, &digest).is_err());
        }
        assert!(outbound_file("one.txt".into(), &encoded, "wrong").is_err());
        assert!(outbound_file("one.txt".into(), "?", &digest).is_err());
        assert!(outbound_file("empty.txt".into(), "", "d41d8cd98f00b204e9800998ecf8427e").is_err());
        let oversized = "x".repeat(MAX_OUTBOUND_FILE_BYTES.div_ceil(3) * 4 + 1);
        assert!(outbound_file("large.txt".into(), &oversized, &digest).is_err());
    }
}
