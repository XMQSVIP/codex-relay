//! Account-owned model time context and approximate search location.
use crate::error::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod detection;
pub(crate) use detection::Manager;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Location {
    pub timezone: String,
    pub country: String,
    pub region: String,
    pub city: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    #[default]
    Proxy,
    Custom,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub enabled: bool,
    pub source: Source,
    pub custom: Location,
}
impl Config {
    pub fn validated(mut self) -> Result<Self> {
        self.custom = self.custom.normalized();
        if [
            &self.custom.timezone,
            &self.custom.country,
            &self.custom.region,
            &self.custom.city,
        ]
        .iter()
        .any(|s| s.chars().count() > 128 || s.chars().any(char::is_control))
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "位置字段最多 128 个字符，且不能包含控制字符。",
            ));
        }
        if self.enabled && self.source == Source::Custom {
            self.custom.validate()?;
        }
        Ok(self)
    }
}
impl Location {
    pub fn normalized(mut self) -> Self {
        self.timezone = self.timezone.trim().into();
        self.country = self.country.trim().to_ascii_uppercase();
        self.region = self.region.trim().into();
        self.city = self.city.trim().into();
        self
    }
    pub fn validate(&self) -> Result<()> {
        if self.timezone.parse::<chrono_tz::Tz>().is_err() {
            return Err(Error::new(
                "INVALID_INPUT",
                "请选择有效的 IANA 时区，例如 America/New_York。",
            ));
        }
        if !self.country.is_empty() && !COUNTRIES.split_whitespace().any(|c| c == self.country) {
            return Err(Error::new("INVALID_INPUT", "请选择有效的国家代码。"));
        }
        if self.country.is_empty() && (!self.region.is_empty() || !self.city.is_empty()) {
            return Err(Error::new(
                "INVALID_INPUT",
                "填写地区或城市时，请先选择国家。",
            ));
        }
        if [&self.region, &self.city]
            .iter()
            .any(|s| s.chars().count() > 128 || s.chars().any(char::is_control))
        {
            return Err(Error::new(
                "INVALID_INPUT",
                "地区和城市不能包含控制字符，且最多 128 个字符。",
            ));
        }
        Ok(())
    }
    pub fn date(&self, now: DateTime<Utc>) -> String {
        now.with_timezone(
            &self
                .timezone
                .parse::<chrono_tz::Tz>()
                .expect("validated location"),
        )
        .format("%Y-%m-%d")
        .to_string()
    }
    pub fn search_location(&self) -> Value {
        let mut value = json!({"timezone":self.timezone});
        for (key, text) in [
            ("country", &self.country),
            ("region", &self.region),
            ("city", &self.city),
        ] {
            if !text.is_empty() {
                value[key] = json!(text);
            }
        }
        value
    }
    pub fn time_item(&self, now: DateTime<Utc>) -> Value {
        json!({"type":"message","role":"developer","content":[{"type":"input_text","text":format!("<codex_apps_client_time_context>\n<timezone>{}</timezone>\n<current_date>{}</current_date>\nUse this account time context for dates and times in this turn.\n</codex_apps_client_time_context>", self.timezone, self.date(now))}]})
    }
    /// Only complete, role-appropriate XML context blocks are rewritten.
    pub fn align_text(&self, text: &str, role: &str, now: DateTime<Utc>) -> String {
        let tag = match role {
            "user" => "environment_context",
            "developer" => "codex_apps_client_time_context",
            _ => return text.into(),
        };
        if !text.trim().starts_with(&format!("<{tag}>"))
            || !text.trim().ends_with(&format!("</{tag}>"))
        {
            return text.into();
        }
        let Ok(doc) = roxmltree::Document::parse(text) else {
            return text.into();
        };
        if !doc.root_element().has_tag_name(tag) {
            return text.into();
        }
        let mut edits: Vec<_> = doc
            .root_element()
            .children()
            .filter(|n| n.is_element())
            .filter_map(|n| {
                let replacement = match n.tag_name().name() {
                    "timezone" => format!("<timezone>{}</timezone>", self.timezone),
                    "current_date" => format!("<current_date>{}</current_date>", self.date(now)),
                    _ => return None,
                };
                Some((n.range(), replacement))
            })
            .collect();
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        let mut result = text.to_owned();
        for (range, replacement) in edits {
            result.replace_range(range, &replacement);
        }
        result
    }
    pub fn align_items(&self, items: &mut Value, now: DateTime<Utc>) {
        let Some(items) = items.as_array_mut() else {
            return;
        };
        for item in items {
            let role = item["role"].as_str().unwrap_or("").to_owned();
            let kinds = item["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                .as_array()
                .cloned();
            if let Some(parts) = item["content"].as_array_mut() {
                for (index, part) in parts.iter_mut().enumerate() {
                    // Explicit content classification takes precedence over text shape.
                    if let Some(kinds) = &kinds {
                        let expected = if role == "user" {
                            "environments.environment_context"
                        } else {
                            "additional_content.codex_apps_client_time_context"
                        };
                        if kinds.get(index).and_then(Value::as_str) != Some(expected) {
                            continue;
                        }
                    }
                    if part["type"] == "input_text"
                        && let Some(text) = part["text"].as_str()
                    {
                        part["text"] = json!(self.align_text(text, &role, now));
                    }
                }
            }
            // Native injected tool declarations bypass client option normalization.
            if item["type"] == "additional_tools"
                && let Some(tools) = item["tools"].as_array_mut()
            {
                for tool in tools {
                    if tool["type"] == "web_search" {
                        let mut location = self.search_location();
                        location["type"] = json!("approximate");
                        tool["user_location"] = location;
                    }
                }
            }
        }
    }
    pub fn apply_thread(&self, params: &mut Value, now: DateTime<Utc>) {
        if !params["config"].is_object() {
            params["config"] = json!({});
        }
        params["config"]["include_environment_context_time"] = json!(false);
        // Location configuration does not enable search; keep the caller's web_search mode.
        params["config"]["tools.web_search.location"] = self.search_location();
        if let Some(text) = params["developerInstructions"].as_str() {
            params["developerInstructions"] = json!(self.align_text(text, "developer", now));
        }
    }
}

pub const COUNTRIES: &str = "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS YE YT ZA ZM ZW";

#[cfg(test)]
mod tests;
