//! A PEM certificate -> the string `CURLOPT_PINNEDPUBLICKEY` accepts (issue #380; used by
//! [`crate::net::keypin`], issue #378).
//!
//! The app remembers each Plex server's certificate public key so an offline TV whose clock is
//! wrong can still recognise its own server. libcurl 7.53.1 reports a verified connection's chain
//! as PEM text (`CURLINFO_CERTINFO`, one `Cert:` entry per certificate); this module is the pure
//! half that turns one of those into `sha256//<base64>`, the format curl documents and the same
//! value as `openssl x509 -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst
//! -sha256 -binary | base64`. What is hashed is the COMPLETE DER `SubjectPublicKeyInfo` (tag,
//! length and contents), not the bare key bits.
//!
//! The one caller is `net::peer_leaf_pin` (the identity probe's `CURLINFO_CERTINFO` read), which
//! hands this module text straight off the network, so the DER reader is bounds-checked end to
//! end and answers `None` rather than panicking.

use crate::keymanager::b64;
use crate::sha256::sha256;

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";

const TAG_INTEGER: u8 = 0x02;
const TAG_SEQUENCE: u8 = 0x30;
/// `[0]` EXPLICIT, constructed: the OPTIONAL `version` that a v1 certificate leaves out.
const TAG_VERSION: u8 = 0xA0;

/// One DER element: `whole` is tag + length + contents, `contents` is the value alone.
struct Tlv<'a> {
    tag: u8,
    whole: &'a [u8],
    contents: &'a [u8],
}

/// Read the element at the front of `buf`; the second half is whatever follows it.
///
/// Definite lengths only: the short form, or the long form with at most four length bytes. An
/// indefinite length (`0x80`), a longer length-of-length, a high-tag-number tag, or a length that
/// runs past `buf` is `None`.
fn read_tlv(buf: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *buf.first()?;
    if tag & 0x1F == 0x1F {
        return None;
    }
    let first = *buf.get(1)?;
    let (header, len) = match first {
        0x00..=0x7F => (2usize, first as usize),
        0x80 => return None,
        0x81..=0x84 => {
            let n = (first & 0x7F) as usize;
            let mut len = 0usize;
            for &b in buf.get(2..2 + n)? {
                len = (len << 8) | b as usize;
            }
            (2 + n, len)
        }
        _ => return None,
    };
    let end = header.checked_add(len)?;
    let whole = buf.get(..end)?;
    Some((
        Tlv {
            tag,
            whole,
            contents: buf.get(header..end)?,
        },
        buf.get(end..)?,
    ))
}

/// Like `read_tlv`, but the element must carry `tag`.
fn expect(buf: &[u8], tag: u8) -> Option<(Tlv<'_>, &[u8])> {
    read_tlv(buf).filter(|(t, _)| t.tag == tag)
}

/// The complete DER `SubjectPublicKeyInfo` of a DER `Certificate`:
/// `SEQUENCE { tbsCertificate SEQUENCE { [0] version OPTIONAL, serialNumber, signature, issuer,
/// validity, subject, subjectPublicKeyInfo, .. }, .. }`.
fn spki_of_certificate(der: &[u8]) -> Option<&[u8]> {
    let (cert, _) = expect(der, TAG_SEQUENCE)?;
    let (tbs, _) = expect(cert.contents, TAG_SEQUENCE)?;
    let mut rest = tbs.contents;
    if rest.first() == Some(&TAG_VERSION) {
        rest = read_tlv(rest)?.1;
    }
    let (_, rest) = expect(rest, TAG_INTEGER)?; // serialNumber
    let mut rest = rest;
    for _ in 0..4 {
        // signature, issuer, validity, subject
        rest = expect(rest, TAG_SEQUENCE)?.1;
    }
    let (spki, _) = expect(rest, TAG_SEQUENCE)?;
    Some(spki.whole)
}

/// The base64 body of the first `BEGIN CERTIFICATE` block in `text`, decoded to DER. Whatever
/// surrounds the block (libcurl's `Cert:` prefix, blank lines, CRLF) is ignored.
fn first_der(text: &str) -> Option<Vec<u8>> {
    let from = text.find(BEGIN)? + BEGIN.len();
    let body = &text[from..from + text[from..].find(END)?];
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    b64::decode(&compact)
}

/// The pin string for a DER `SubjectPublicKeyInfo`.
pub(crate) fn pin_from_spki_der(spki: &[u8]) -> String {
    format!("sha256//{}", b64::encode(&sha256(spki)))
}

/// The `CURLOPT_PINNEDPUBLICKEY` string for the first certificate in `pem`, or `None` when there
/// is no certificate or it is not well-formed X.509.
pub(crate) fn pin_from_pem(pem: &str) -> Option<String> {
    let der = first_der(pem)?;
    Some(pin_from_spki_der(spki_of_certificate(&der)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::PublicKeyData;

    /// Throwaway self-signed RSA-2048 certificate (v3, one SAN), minted once with the host
    /// `openssl` CLI for this test alone. Its key was discarded; it is not any server's.
    const RSA_V3_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIC3TCCAcWgAwIBAgIJAJSROovjnCDXMA0GCSqGSIb3DQEBCwUAMBwxGjAYBgNV
BAMMEXNwa2ktdGVzdC5pbnZhbGlkMCAXDTI2MTAwMTIzMjYwNFoYDzIxMjYwOTA3
MjMyNjA0WjAcMRowGAYDVQQDDBFzcGtpLXRlc3QuaW52YWxpZDCCASIwDQYJKoZI
hvcNAQEBBQADggEPADCCAQoCggEBALSde8zzTWUetNeUJUIiRVnLUdD3dr/xF0bM
AJL3lfpiFCnZq8OvlyNS4RUVerDu0XQK+H3PfTu4akwJoQ/+ITnhfmzhB7zyih67
2AK7Ir+KLgocFBct3RXW/4NniejTeBs3uHNkamAiNdIzFxPfYN1CYserXpZhAEod
6baWuauaWBAA9UeBFNSIfXp9RBgOy6ZdcQgomHIx3wa9eskw8lKvBlXWSWpFJC8L
nEDoZUoPTCCBnnCtJ2NP9mULXzaPKhcBOTCZ6AhGnxld67fUzuPUOhnoEkmT7xFc
STFM/OB85bZrVZ8PSkdn9L1RmKhL+2n4vj3A7J6Jvl2xvPqYZJ0CAwEAAaMgMB4w
HAYDVR0RBBUwE4IRc3BraS10ZXN0LmludmFsaWQwDQYJKoZIhvcNAQELBQADggEB
ALAZR4ZTlPzw9OFshVhdeuA8Bd830Jap8Qzxd5+fdRVOD3CuLKoFl5/H5IHByMUJ
bw/u1LjXPG8M5njnwfYntGMBrTTKKUHJ13Gc4WrZcfqYd7bUFdiL9zsBHDWTa7A7
FEtTqqsOg6+BOt0jAb35h3hQLftEAcFFt0C+KOJEBQ3ye5HxT6TRZrOuatUUrHJy
EM37CBaCLnsrsWSUcLF3d2QfaDIUe9tk+GOzmsYzOweqQnmKUFRIl8XloRe1p+sO
XtS7pjh7a2kkRsembr7RtJAm30gJiw3NDu7gQG2ZbDt87qPzh2V52JKxVloUsDU+
Vqrg749PmJliNqekFlfz3E8=
-----END CERTIFICATE-----";
    /// `openssl x509 -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256
    /// -binary | base64` over `RSA_V3_PEM`.
    const RSA_V3_PIN: &str = "sha256//TGi1vAgbpW8l+xg62rRZ7yAFx5iBzdm9hxaSDv8ysnc=";

    /// The same, but a VERSION 1 certificate (no `[0]` version element, no extensions).
    const RSA_V1_PEM: &str = "-----BEGIN CERTIFICATE-----
MIICtjCCAZ4CCQCHhYqf+rrs5zANBgkqhkiG9w0BAQsFADAcMRowGAYDVQQDDBFz
cGtpLXRlc3QuaW52YWxpZDAgFw0yNjEwMDEyMzI2MDRaGA8yMTI2MDkwNzIzMjYw
NFowHDEaMBgGA1UEAwwRc3BraS10ZXN0LmludmFsaWQwggEiMA0GCSqGSIb3DQEB
AQUAA4IBDwAwggEKAoIBAQDaMafKJ1x+WWxzqNdzp2+5X0L3Xgz1q06FPAiaO+nQ
NeIMRKCdkUgmdPQSizOwSSC5QovwzMDGw83eWt0wIpHevG45SFesyjc/kwKYcTIP
V+QnQHaD0wVLCArn5ZM+T54vKH7Pg9Bk4y90ovI7iRnLdb438N706TSvL/LSX8+p
vH3UlQ7ZXGOINi3bpV4RwoMZ92Ewz+0W7XMX4XcCJpxbCUbEiqnhUVv97wDRBwOd
ZxErQZ7Jb9SmfraLSkotanRoe7u4CHFJZmo+WpEw2ePqHqufaBKuWMJ6sKGaRZ4a
f628rahrXRNmKouMS7X4zSYks/OHuatVieKnYE5PK0n3AgMBAAEwDQYJKoZIhvcN
AQELBQADggEBADsTW8SeJ6Y3+1ke7VHidoAQ07v1033jF8yVVcB0fWXXDXg5bV5X
K0E73ah0WjM5GZHhWNwnwqwcmrTwNx5qR658V2Phct43VtJeYNZ+0dn0H2bAGmqb
Ayir6sLTLuIUED+xy96SCWemzxqimvhVN4EWjo7VVbvT/FxuPnZ4I670PA9Lpa6B
vAp1FDbiH9Ghz5JTEao1tJ4P2vgts5IOWV6/2CNHJ+x/7HCT2/pNkupcOBdx6zSC
ut3ATYXqn0OjJ6uMV3yepdkAaCFT9k/J6PaWXSSw1O8/jTy27a8DWoT9Hzg0Q0Cp
1FljQZ32d1T1+exhi+CWL9wikfGRWeIraVM=
-----END CERTIFICATE-----";
    const RSA_V1_PIN: &str = "sha256//lxHbsdBagVWf0HiGKkyesh0TVeHBS+BttBPIQUsIXik=";

    fn pem_of(der: &[u8]) -> String {
        let body = b64::encode(der);
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!("{BEGIN}\n{}\n{END}\n", lines.join("\n"))
    }

    fn der_of(pem: &str) -> Vec<u8> {
        first_der(pem).unwrap()
    }

    fn minted(alg: &'static rcgen::SignatureAlgorithm) -> (String, Vec<u8>) {
        let key = rcgen::KeyPair::generate_for(alg).unwrap();
        let cert = rcgen::CertificateParams::new(vec!["spki.invalid".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert.pem(), key.subject_public_key_info())
    }

    #[test]
    fn ecdsa_p256_pin_matches_the_key_pairs_own_spki() {
        let (pem, spki) = minted(&rcgen::PKCS_ECDSA_P256_SHA256);
        assert_eq!(pin_from_pem(&pem).unwrap(), pin_from_spki_der(&spki));
    }

    #[test]
    fn ed25519_pin_matches_the_key_pairs_own_spki() {
        let (pem, spki) = minted(&rcgen::PKCS_ED25519);
        assert_eq!(pin_from_pem(&pem).unwrap(), pin_from_spki_der(&spki));
    }

    #[test]
    fn rsa_2048_pin_matches_the_openssl_pipeline() {
        assert_eq!(pin_from_pem(RSA_V3_PEM).as_deref(), Some(RSA_V3_PIN));
    }

    #[test]
    fn a_v1_certificate_without_the_version_element_still_parses() {
        let der = der_of(RSA_V1_PEM);
        // The first element inside tbsCertificate is the serial INTEGER, not `[0]`.
        let (cert, _) = expect(&der, TAG_SEQUENCE).unwrap();
        let (tbs, _) = expect(cert.contents, TAG_SEQUENCE).unwrap();
        assert_eq!(tbs.contents[0], TAG_INTEGER);
        assert_eq!(pin_from_pem(RSA_V1_PEM).as_deref(), Some(RSA_V1_PIN));
    }

    #[test]
    fn crlf_line_endings_and_surrounding_text_are_tolerated() {
        let wrapped = format!(
            "Cert:  \r\n{}\r\nSubject:CN=x\r\n",
            RSA_V3_PEM.replace('\n', "\r\n")
        );
        assert_eq!(pin_from_pem(&wrapped).as_deref(), Some(RSA_V3_PIN));
        assert_eq!(
            pin_from_pem(&format!("\n\n  {RSA_V3_PEM}  \n\n")).as_deref(),
            Some(RSA_V3_PIN)
        );
    }

    #[test]
    fn the_first_of_several_blocks_wins() {
        let two = format!("{RSA_V3_PEM}\n{RSA_V1_PEM}\n");
        assert_eq!(pin_from_pem(&two).as_deref(), Some(RSA_V3_PIN));
        let swapped = format!("{RSA_V1_PEM}\n{RSA_V3_PEM}\n");
        assert_eq!(pin_from_pem(&swapped).as_deref(), Some(RSA_V1_PIN));
    }

    #[test]
    fn text_that_is_not_a_certificate_is_none() {
        assert_eq!(pin_from_pem(""), None);
        assert_eq!(pin_from_pem("no markers here"), None);
        assert_eq!(pin_from_pem(BEGIN), None, "no END marker");
        assert_eq!(pin_from_pem(&format!("{BEGIN}\n{END}")), None, "empty body");
        assert_eq!(
            pin_from_pem(&format!("{BEGIN}\n!!!!\n{END}")),
            None,
            "bad base64"
        );
        assert_eq!(
            pin_from_pem(&format!("{BEGIN}\nQUJD\n{END}")),
            None,
            "base64 of 'ABC'"
        );
        assert_eq!(
            pin_from_pem(&format!("{END}\n{BEGIN}")),
            None,
            "markers reversed"
        );
    }

    #[test]
    fn every_truncation_of_a_valid_certificate_is_none() {
        for pem in [RSA_V3_PEM, RSA_V1_PEM] {
            let der = der_of(pem);
            for n in 0..der.len() {
                assert_eq!(spki_of_certificate(&der[..n]), None, "prefix of {n} bytes");
                assert_eq!(pin_from_pem(&pem_of(&der[..n])), None, "pem of {n} bytes");
            }
            assert!(spki_of_certificate(&der).is_some());
        }
    }

    #[test]
    fn a_length_past_the_buffer_is_none() {
        assert_eq!(spki_of_certificate(&[0x30, 0x05, 0x30, 0x00]), None);
        assert_eq!(
            spki_of_certificate(&[0x30, 0x82, 0xFF, 0xFF, 0x30, 0x00]),
            None
        );
        // A 4-byte length of 0xFFFFFFFF must not overflow the end offset on any target.
        assert_eq!(
            spki_of_certificate(&[0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF, 0x30, 0x00]),
            None
        );
    }

    #[test]
    fn an_indefinite_length_is_none() {
        assert_eq!(
            spki_of_certificate(&[0x30, 0x80, 0x30, 0x00, 0x00, 0x00]),
            None
        );
    }

    #[test]
    fn a_five_byte_length_is_none() {
        assert_eq!(
            spki_of_certificate(&[0x30, 0x85, 0x00, 0x00, 0x00, 0x00, 0x02, 0x30, 0x00]),
            None
        );
        assert!(read_tlv(&[0x30, 0xFF, 0x00]).is_none());
    }

    #[test]
    fn a_high_tag_number_is_none() {
        assert!(read_tlv(&[0x1F, 0x01, 0x00]).is_none());
    }

    #[test]
    fn a_certificate_missing_its_spki_is_none() {
        // tbsCertificate stops after `subject`.
        let tbs = [
            &[0x02, 0x01, 0x01][..],
            &[0x30, 0x00],
            &[0x30, 0x00],
            &[0x30, 0x00],
            &[0x30, 0x00],
        ]
        .concat();
        let mut der = vec![0x30, (tbs.len() + 2) as u8, 0x30, tbs.len() as u8];
        der.extend_from_slice(&tbs);
        assert_eq!(spki_of_certificate(&der), None);
    }

    /// Hand-built minimal certificate whose every length uses the 4-byte long form, so the
    /// widest branch of the reader is exercised and not just the lengths openssl chooses.
    #[test]
    fn four_byte_long_form_lengths_parse() {
        fn tlv4(tag: u8, contents: &[u8]) -> Vec<u8> {
            let mut v = vec![tag, 0x84];
            v.extend_from_slice(&(contents.len() as u32).to_be_bytes());
            v.extend_from_slice(contents);
            v
        }
        let spki = tlv4(TAG_SEQUENCE, &[0x05, 0x00, 0x03, 0x01, 0x00]);
        let tbs = [
            tlv4(TAG_VERSION, &[0x02, 0x01, 0x02]),
            tlv4(TAG_INTEGER, &[0x07]),
            tlv4(TAG_SEQUENCE, &[]),
            tlv4(TAG_SEQUENCE, &[]),
            tlv4(TAG_SEQUENCE, &[]),
            tlv4(TAG_SEQUENCE, &[]),
            spki.clone(),
        ]
        .concat();
        let der = tlv4(TAG_SEQUENCE, &tlv4(TAG_SEQUENCE, &tbs));
        assert_eq!(spki_of_certificate(&der), Some(&spki[..]));
        assert_eq!(
            pin_from_pem(&pem_of(&der)).unwrap(),
            pin_from_spki_der(&spki)
        );
    }

    #[test]
    fn the_pin_has_curls_documented_shape() {
        let pin = pin_from_spki_der(b"anything");
        let b = pin.strip_prefix("sha256//").unwrap();
        assert_eq!(
            b.len(),
            44,
            "32 bytes of digest -> 44 padded base64 characters"
        );
        assert!(b.ends_with('='));
    }
}
