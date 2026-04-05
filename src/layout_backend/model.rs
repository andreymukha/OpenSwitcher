use super::error::LayoutCodeNormalizationError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppLayoutKind {
    English,
    Russian,
    Other,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NormalizedLayoutCode(String);

impl NormalizedLayoutCode {
    pub fn new(value: impl Into<String>) -> Result<Self, LayoutCodeNormalizationError> {
        let value = value.into();
        if !is_valid_normalized_layout_code(&value) {
            return Err(LayoutCodeNormalizationError { value });
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutCode {
    Us,
    Ru,
    Other(NormalizedLayoutCode),
    Unknown,
}

impl LayoutCode {
    pub fn from_normalized(value: &str) -> Result<Self, LayoutCodeNormalizationError> {
        match value {
            "us" => Ok(Self::Us),
            "ru" => Ok(Self::Ru),
            "unknown" => Ok(Self::Unknown),
            other => NormalizedLayoutCode::new(other.to_string()).map(Self::Other),
        }
    }

    pub fn normalized_str(&self) -> Option<&str> {
        match self {
            Self::Us => Some("us"),
            Self::Ru => Some("ru"),
            Self::Other(value) => Some(value.as_str()),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemLayout {
    pub backend_key: String,
    pub normalized_code: LayoutCode,
    pub display_name: String,
    pub kind: AppLayoutKind,
    pub index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentLayoutState {
    Known {
        layout: SystemLayout,
        trustworthy: bool,
    },
    Unknown {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutSetup {
    StrictPair {
        en: SystemLayout,
        ru: SystemLayout,
    },
    PairPlusOther {
        en: SystemLayout,
        ru: SystemLayout,
        others: Vec<SystemLayout>,
    },
    Unsupported {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutCompatibility {
    FullStrictPair,
    PairPlusOther,
    Limited,
    Unsupported,
}

fn is_valid_normalized_layout_code(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }

    value
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_known_normalized_layout_codes() {
        assert_eq!(LayoutCode::from_normalized("us").unwrap(), LayoutCode::Us);
        assert_eq!(LayoutCode::from_normalized("ru").unwrap(), LayoutCode::Ru);
    }

    #[test]
    fn accepts_normalized_other_layout_codes_only() {
        let code = LayoutCode::from_normalized("de").unwrap();
        assert_eq!(
            code,
            LayoutCode::Other(NormalizedLayoutCode::new("de").unwrap())
        );
    }

    #[test]
    fn rejects_raw_backend_specific_layout_codes() {
        assert!(LayoutCode::from_normalized("German (DE)").is_err());
        assert!(LayoutCode::from_normalized("xkb:us::eng").is_err());
    }
}
