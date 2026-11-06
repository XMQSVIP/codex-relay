//! HTTP media types are case insensitive and may include parameters.

pub(crate) fn content_encoding(headers: &axum::http::HeaderMap) -> crate::error::Result<&str> {
    use axum::http::header::CONTENT_ENCODING;
    if headers.get_all(CONTENT_ENCODING).iter().count() > 1 {
        return Err(crate::error::Error::new(
            "UNSUPPORTED_MEDIA_TYPE",
            "Content-Encoding 不接受多层或重复编码。",
        ));
    }
    let encoding = headers.get(CONTENT_ENCODING).map_or(Ok(""), |value| {
        value.to_str().map_err(|_| {
            crate::error::Error::new("UNSUPPORTED_MEDIA_TYPE", "Content-Encoding 无效。")
        })
    })?;
    if encoding.contains(',') {
        return Err(crate::error::Error::new(
            "UNSUPPORTED_MEDIA_TYPE",
            "Content-Encoding 不接受多层或重复编码。",
        ));
    }
    Ok(encoding)
}

pub(crate) fn is_type(content_type: &str, expected: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case(expected))
}

pub(crate) fn is_json(content_type: &str) -> bool {
    content_type.split(';').next().is_some_and(|media_type| {
        let media_type = media_type.trim().to_ascii_lowercase();
        media_type == "application/json"
            || (media_type.starts_with("application/") && media_type.ends_with("+json"))
    })
}

#[cfg(test)]
mod tests {
    use super::is_type;

    #[test]
    fn accepts_case_and_parameters_but_requires_the_exact_media_type() {
        for content_type in [
            "text/event-stream",
            "Text/Event-Stream",
            "TEXT/EVENT-STREAM; charset=utf-8",
            "text/event-stream ; charset=utf-8",
        ] {
            assert!(is_type(content_type, "text/event-stream"), "{content_type}");
        }
        for content_type in ["", "text/event-streaming", "application/json"] {
            assert!(
                !is_type(content_type, "text/event-stream"),
                "{content_type}"
            );
        }
        assert!(is_type(
            "Application/JSON; charset=UTF-8",
            "application/json"
        ));
        assert!(!is_type("application/jsonp", "application/json"));
    }
}
