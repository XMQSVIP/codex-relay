//! Bounded inline media only; callers cannot name host files or fetch URLs.
use crate::error::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
pub const BODY_LIMIT: usize = 32 * 1024 * 1024;
pub const MEDIA_LIMIT: usize = 16 * 1024 * 1024;
pub const IMAGE_LIMIT: usize = 4 * 1024 * 1024;
pub const IMAGE_COUNT: usize = 32;
pub const HISTORY_COUNT: usize = 16_384;
pub const TOOL_COUNT: usize = 1_024;
pub fn inline_image(url: &str) -> Result<usize> {
    let invalid = || {
        Error::new(
            "INVALID_INPUT",
            "图片须为 PNG/JPEG/WebP 的 base64 data URL，单张不超过 4 MiB。不接受文件路径或远程 URL。",
        )
    };
    let (mime, data) = url.split_once(";base64,").ok_or_else(invalid)?;
    if !["data:image/png", "data:image/jpeg", "data:image/webp"].contains(&mime)
        || data.len() > IMAGE_LIMIT * 4 / 3 + 4
    {
        return Err(invalid());
    }
    let bytes = STANDARD.decode(data).map_err(|_| invalid())?;
    let valid = match mime {
        "data:image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "data:image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        _ => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
    };
    if !valid || bytes.len() > IMAGE_LIMIT {
        return Err(invalid());
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes));
    reader.set_format(match mime {
        "data:image/png" => image::ImageFormat::Png,
        "data:image/jpeg" => image::ImageFormat::Jpeg,
        _ => image::ImageFormat::WebP,
    });
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    reader.decode().map_err(|_| invalid())?;
    Ok(bytes.len())
}
pub fn budget(value: &Value, text_limit: usize) -> Result<bool> {
    fn walk(v: &Value, media: &mut usize, count: &mut usize, text: &mut usize) -> Result<()> {
        match v {
            Value::Object(map) => {
                for (k, v) in map {
                    if (k == "image_url" && map.get("type").is_some_and(|t| t == "input_image"))
                        || (k == "url" && map.get("type").is_some_and(|t| t == "image"))
                    {
                        *media += inline_image(
                            v.as_str()
                                .ok_or_else(|| Error::new("INVALID_INPUT", "图片内容无效。"))?,
                        )?;
                        *count += 1;
                    } else {
                        *text += k.len();
                        walk(v, media, count, text)?;
                    }
                }
            }
            Value::Array(a) => {
                for v in a {
                    walk(v, media, count, text)?;
                }
            }
            Value::String(s) => *text += s.len(),
            _ => {}
        }
        Ok(())
    }
    let (mut media, mut count, mut text) = (0, 0, 0);
    walk(value, &mut media, &mut count, &mut text)?;
    if media > MEDIA_LIMIT
        || count > IMAGE_COUNT
        || text > text_limit
        || serde_json::to_vec(value)?.len() > BODY_LIMIT
    {
        return Err(Error::new(
            "PAYLOAD_TOO_LARGE",
            &format!(
                "容量超限：正文/历史最多 32 MiB，图片最多 {IMAGE_COUNT} 张、共 16 MiB（当前图片 {count} 张、{media} 字节，文本 {text} 字节）。"
            ),
        ));
    }
    Ok(count > 0)
}

pub fn generated_image(encoded: &str) -> Result<()> {
    if encoded.len() > IMAGE_LIMIT * 4 / 3 + 4 {
        return Err(Error::new("MEDIA_OUTPUT_LIMIT", "生成图片超过 4 MiB。"));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| Error::new("UPSTREAM_INVALID_RESPONSE", "原生图片不是有效 base64。"))?;
    if bytes.is_empty() || bytes.len() > IMAGE_LIMIT {
        return Err(Error::new(
            "MEDIA_OUTPUT_LIMIT",
            "生成图片为空或超过 4 MiB。",
        ));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| Error::new("UPSTREAM_INVALID_RESPONSE", "无法识别原生图片格式。"))?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP)
    ) {
        return Err(Error::new(
            "UPSTREAM_INVALID_RESPONSE",
            "原生图片格式不支持。",
        ));
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|_| Error::new("UPSTREAM_INVALID_RESPONSE", "原生图片无法解码。"))?;
    Ok(())
}
pub fn validate_artifact(root: &std::path::Path, file: &std::path::Path) -> Result<()> {
    let root = std::fs::canonicalize(root)?;
    let file = std::fs::canonicalize(file)?;
    if !file.starts_with(root)
        || !file.is_file()
        || std::fs::metadata(file)?.len() > IMAGE_LIMIT as u64
    {
        return Err(Error::new(
            "MEDIA_ARTIFACT_SCOPE",
            "原生媒体不属于当前请求目录或超过容量上限。",
        ));
    }
    Ok(())
}
