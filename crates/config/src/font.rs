use std::fmt;
use std::rc::Rc;

use std::hash::{Hash, Hasher};
use utils::floats::NotNan;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidData;

impl std::error::Error for InvalidData {}

impl fmt::Display for InvalidData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid data")
    }
}

// Types related to fonts
#[derive(Eq, PartialEq)]
pub struct Variation {
    pub tag: Tag,
    pub value: NotNan<f64>,
}

impl Hash for Variation {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.tag.hash(state);
        self.value.hash(state);
    }
}

impl Variation {
    #[inline]
    pub fn parse(input: &str) -> Result<Self, InvalidData> {
        let (key_str, value_str) = input.split_once('=').ok_or(InvalidData)?;
        let key = key_str.trim_matches([' ', '\t']);
        let value = value_str.trim_matches([' ', '\t']);
        if key.len() != 4 {
            return Err(InvalidData);
        }
        let tag = Tag::new(key.as_bytes().try_into().or(Err(InvalidData))?);
        let value = NotNan::new(value.parse().or(Err(InvalidData))?).ok_or(InvalidData)?;

        Ok(Variation { tag, value })
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct Tag([u8; 4]);

impl Tag {
    /// The input should be in little endian
    #[inline]
    pub const fn new(variation: [u8; 4]) -> Self {
        Self(variation)
    }

    #[inline]
    pub const fn packed(self) -> u32 {
        u32::from_ne_bytes(self.0)
    }
}

// TODO: Doc comments
#[derive(Clone, Eq, PartialEq)]
pub enum FontStyle {
    Default,
    False,
    Name(Rc<str>),
}

impl FontStyle {
    #[inline]
    pub fn name_value(&self) -> Option<Rc<str>> {
        match self {
            FontStyle::Name(name) => Some(Rc::clone(name)),
            FontStyle::Default | FontStyle::False => None,
        }
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        *self != FontStyle::False
    }
}
