//! Avatar pictures (ADR-037): any JPEG or PNG up to 5 MB becomes a 512×512
//! JPEG without EXIF, stored in Postgres under a fresh random ID.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbImage};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::config::Config;

/// Upload limit, 5 MB (DESIGN §15).
pub const MAX_BYTES: usize = 5 * 1024 * 1024;
/// Stored size, in pixels per side.
pub const SIDE: u32 = 512;
/// Changes of personal and group avatars per account per day (DESIGN §15).
pub const PER_DAY: usize = 20;
/// Largest picture once decoded, checked before decoding: a PNG of a few
/// hundred KB can claim 8192×8192 at 16 bits a channel (DESIGN §15).
pub const MAX_DECODED: u64 = 64 * 1024 * 1024;
const QUALITY: u8 = 85;

/// Why a picture can't become an avatar.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// Not a JPEG or PNG we can decode.
    NotImage,
    /// More than `MAX_DECODED` bytes of pixels.
    TooManyPixels,
}

/// The address of an avatar (API-047): on `IMBO_DOMAIN` when set, else a
/// path the client puts after the server address it uses (API-045).
pub fn url(cfg: &Config, id: Uuid) -> String {
    match &cfg.domain {
        Some(d) => format!("https://{d}/v1/avatars/{id}.jpg"),
        None => format!("/v1/avatars/{id}.jpg"),
    }
}

pub fn url_of(cfg: &Config, id: Option<Uuid>) -> Option<String> {
    id.map(|id| url(cfg, id))
}

/// JPEG or PNG by content; `None` for anything else.
fn format_of(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => Some(ImageFormat::Jpeg),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => Some(ImageFormat::Png),
        _ => None,
    }
}

/// Turn upright by EXIF, take the centre square, scale to 512×512, flatten
/// transparency onto white and encode as a JPEG with no metadata (INV-024).
pub fn normalize(bytes: &[u8]) -> Result<Vec<u8>, Refused> {
    let format = format_of(bytes).ok_or(Refused::NotImage)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| Refused::NotImage)?;
    if decoder.total_bytes() > MAX_DECODED {
        return Err(Refused::TooManyPixels);
    }
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|_| Refused::NotImage)?;
    img.apply_orientation(orientation);
    let side = img.width().min(img.height());
    if side == 0 {
        return Err(Refused::NotImage);
    }
    let square = img.crop_imm(
        (img.width() - side) / 2,
        (img.height() - side) / 2,
        side,
        side,
    );
    let small = square.resize_exact(SIDE, SIDE, FilterType::Lanczos3);
    let rgb = if small.color().has_alpha() {
        on_white(&small)
    } else {
        small.to_rgb8()
    };
    let mut out = vec![];
    JpegEncoder::new_with_quality(&mut out, QUALITY)
        .encode_image(&rgb)
        .map_err(|_| Refused::NotImage)?;
    Ok(out)
}

fn on_white(img: &DynamicImage) -> RgbImage {
    let rgba = img.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let mix = |c: u8| ((c as u16 * a as u16 + 255 * (255 - a as u16)) / 255) as u8;
        image::Rgb([mix(r), mix(g), mix(b)])
    })
}

/// Store a picture under a new random ID (ADR-037).
pub async fn insert(tx: &mut PgConnection, jpeg: Vec<u8>) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO avatars (id, jpeg) VALUES ($1, $2)")
        .bind(id)
        .bind(jpeg)
        .execute(&mut *tx)
        .await?;
    Ok(id)
}

/// Delete pictures; whatever pointed at them now has none.
pub async fn delete(tx: &mut PgConnection, ids: &[Uuid]) -> Result<(), sqlx::Error> {
    if !ids.is_empty() {
        sqlx::query("DELETE FROM avatars WHERE id = ANY($1)")
            .bind(ids)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use image::{GenericImageView, ImageEncoder, Rgb, Rgba, RgbaImage};

    use super::*;

    fn jpeg(img: &RgbImage, exif: Option<Vec<u8>>) -> Vec<u8> {
        let mut out = vec![];
        let mut enc = JpegEncoder::new_with_quality(&mut out, 95);
        if let Some(exif) = exif {
            enc.set_exif_metadata(exif).unwrap();
        }
        enc.write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
        out
    }

    /// A little-endian TIFF block with Orientation and a GPS pointer.
    fn exif(orientation: u16) -> Vec<u8> {
        let mut e = b"II\x2A\x00\x08\x00\x00\x00".to_vec();
        e.extend_from_slice(&2u16.to_le_bytes());
        // 0x0112 Orientation, SHORT, 1, value
        e.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0]);
        e.extend_from_slice(&orientation.to_le_bytes());
        e.extend_from_slice(&[0, 0]);
        // 0x8825 GPSInfo, LONG, 1, offset 38
        e.extend_from_slice(&[0x25, 0x88, 4, 0, 1, 0, 0, 0, 38, 0, 0, 0]);
        e.extend_from_slice(&[0, 0, 0, 0]);
        // GPS IFD: one entry, 0x0001 GPSLatitudeRef = "N"
        e.extend_from_slice(&1u16.to_le_bytes());
        e.extend_from_slice(&[1, 0, 2, 0, 2, 0, 0, 0, b'N', 0, 0, 0]);
        e.extend_from_slice(&[0, 0, 0, 0]);
        e
    }

    fn decode(bytes: &[u8]) -> DynamicImage {
        image::load_from_memory_with_format(bytes, ImageFormat::Jpeg).unwrap()
    }

    fn near(p: Rgb<u8>, want: [u8; 3]) -> bool {
        p.0.iter().zip(want).all(|(a, b)| a.abs_diff(b) < 40)
    }

    #[test]
    fn landscape_becomes_centre_square_without_exif() {
        // Left third red, middle green, right third blue.
        let img = RgbImage::from_fn(900, 300, |x, _| match x / 300 {
            0 => Rgb([255, 0, 0]),
            1 => Rgb([0, 255, 0]),
            _ => Rgb([0, 0, 255]),
        });
        let out = normalize(&jpeg(&img, Some(exif(1)))).unwrap();
        assert!(!out.windows(4).any(|w| w == b"Exif"));
        let back = decode(&out);
        assert_eq!(back.dimensions(), (SIDE, SIDE));
        assert!(near(
            back.to_rgb8().get_pixel(256, 256).to_owned(),
            [0, 255, 0]
        ));
    }

    #[test]
    fn exif_orientation_is_applied() {
        // Orientation 6 means "rotate 90° clockwise to show", so the stored
        // left half (red) is the top once upright, the right half the bottom.
        let stored = RgbImage::from_fn(400, 200, |x, _| {
            if x < 200 {
                Rgb([255, 0, 0])
            } else {
                Rgb([0, 0, 255])
            }
        });
        let back = decode(&normalize(&jpeg(&stored, Some(exif(6)))).unwrap()).to_rgb8();
        assert!(near(*back.get_pixel(256, 40), [255, 0, 0]));
        assert!(near(*back.get_pixel(256, 470), [0, 0, 255]));
    }

    #[test]
    fn small_png_with_alpha_is_scaled_up_on_white() {
        let img = RgbaImage::from_pixel(10, 10, Rgba([0, 0, 0, 0]));
        let mut png = vec![];
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(img.as_raw(), 10, 10, image::ExtendedColorType::Rgba8)
            .unwrap();
        let back = decode(&normalize(&png).unwrap());
        assert_eq!(back.dimensions(), (SIDE, SIDE));
        assert!(near(*back.to_rgb8().get_pixel(5, 5), [255, 255, 255]));
    }

    #[test]
    fn other_bytes_are_refused() {
        assert_eq!(normalize(b"GIF89a...."), Err(Refused::NotImage));
        assert_eq!(
            normalize(&[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3]),
            Err(Refused::NotImage)
        );
        assert_eq!(
            normalize(b"\x89PNG\r\n\x1a\nbroken"),
            Err(Refused::NotImage)
        );
        assert_eq!(normalize(b""), Err(Refused::NotImage));
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }

    /// A PNG that only claims its size: header, an empty image stream, end.
    fn png_claiming(side: u32, bit_depth: u8) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            png.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let start = png.len();
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            let crc = crc32(&png[start..]);
            png.extend_from_slice(&crc.to_be_bytes());
        };
        let mut ihdr = side.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&side.to_be_bytes());
        // RGBA, deflate, adaptive filtering, not interlaced
        ihdr.extend_from_slice(&[bit_depth, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        chunk(b"IEND", &[]);
        png
    }

    #[test]
    fn small_png_claiming_huge_size_is_refused_before_decoding() {
        // 8192×8192 RGBA at 16 bits: 512 MiB of pixels in under 100 bytes.
        let png = png_claiming(8192, 16);
        assert!(png.len() < 100);
        assert_eq!(normalize(&png), Err(Refused::TooManyPixels));
        // 2048×2048 RGBA at 8 bits (16 MiB) passes the check and then
        // fails to decode, as its pixel data is missing.
        assert_eq!(normalize(&png_claiming(2048, 8)), Err(Refused::NotImage));
    }
}
