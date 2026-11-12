// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What the readers of Cobblemon's JSON files share.
use horizon_protocol::{Error, Result};

/// Reads `text` as a `T`; `what` it is starts the error. A byte order mark is skipped, as Gson skips it.
///
/// A file that is not strict JSON is read again as Gson's lenient reader takes it ([`lenient`]): resource
/// packs made for Cobblemon carry such files, and the game reads them. The error, when that fails too, is
/// the strict reader's.
pub(crate) fn parse<T: serde::de::DeserializeOwned>(text: &str, what: &str) -> Result<T> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_json::from_str(text).or_else(|strict| {
        serde_json::from_str(&lenient(text)).map_err(|_| Error::new(format!("{what}: {strict}")))
    })
}

/// `text` as strict JSON where it only differs by what Gson's lenient reader lets through and Cobblemon's
/// resource packs do: a number without a digit before or after its point (`.5`, `0.`), a tab or a line
/// break inside a string, a comma before a closing bracket, and comments.
pub(crate) fn lenient(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if in_string {
            match c {
                '\\' => {
                    out.push(c);
                    if let Some(escaped) = next {
                        out.push(escaped);
                        i += 1;
                    }
                }
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if next == Some('/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if next == Some('*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
                continue;
            }
            // The comma before it goes, whatever blanks and comments stood between the two: the
            // comments are gone from `out` by now, and a string ends in its quote, never in a comma.
            ']' | '}' => {
                let kept = out.trim_end().len();
                if out[..kept].ends_with(',') {
                    out.truncate(kept - 1);
                }
                out.push(c);
            }
            '.' => {
                let digit_before = out.chars().next_back().is_some_and(|p| p.is_ascii_digit());
                if !digit_before {
                    out.push('0');
                }
                out.push('.');
                if !next.is_some_and(|n| n.is_ascii_digit()) {
                    out.push('0');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

/// Makes these structs read from a JSON object and from nothing else: a struct serde derives also reads an
/// array, as its fields in order, which Gson never does. Each derives with `#[serde(remote = "Self")]`, so
/// that what serde derives is a plain function for this to call once it has seen the object.
macro_rules! object_only {
    ($($name:ident),+) => {$(
        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                struct Object;
                impl<'de> serde::de::Visitor<'de> for Object {
                    type Value = $name;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("an object")
                    }
                    fn visit_map<A: serde::de::MapAccess<'de>>(
                        self,
                        map: A,
                    ) -> std::result::Result<$name, A::Error> {
                        $name::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    }
                }
                deserializer.deserialize_map(Object)
            }
        }
    )+};
}
pub(crate) use object_only;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_gson_lets_through_is_read() {
        let read = |text: &str| parse::<serde_json::Value>(text, "test");
        // Strict JSON is untouched, a dot or a comma inside a string too.
        assert_eq!(
            read(r#"{"a": "x.y, ]", "b": [1.5]}"#).unwrap()["a"],
            "x.y, ]"
        );
        assert_eq!(read(r#"{"scale": .3}"#).unwrap()["scale"], 0.3);
        assert_eq!(read(r#"{"at": [0., 0.1, -.5]}"#).unwrap()["at"][2], -0.5);
        assert_eq!(read("{\"q\": \"a\tb\"}").unwrap()["q"], "a\tb");
        assert_eq!(
            read("{\"a\": [1, 2,], // note\n \"b\": 3, /* more */ }").unwrap()["b"],
            3
        );
        // A string that ends in a comma keeps it.
        assert_eq!(read("{\"a\": [\"x,\" ], \"b\": .5}").unwrap()["a"][0], "x,");
        // What is no JSON either way keeps the strict reader's error.
        assert!(read("{").unwrap_err().0.starts_with("test: EOF"));
    }
}
