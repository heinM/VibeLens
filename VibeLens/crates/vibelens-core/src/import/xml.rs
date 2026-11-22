//! A small read-only XML tree over `quick-xml`, enough for JetBrains' `dataSources.xml`,
//! `dataSources.local.xml` and `sshConfigs.xml`.

use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Debug, Default, Clone)]
pub(super) struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<El>,
    pub text: String,
}

impl El {
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn child(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Trimmed, non-empty text of the first child called `name`.
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name)
            .map(|c| c.text.trim())
            .filter(|t| !t.is_empty())
    }

    /// Every element called `name` below this one (depth first).
    pub fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a El>) {
        for c in &self.children {
            if c.name == name {
                out.push(c);
            }
            c.descendants(name, out);
        }
    }

    pub fn find_all(&self, name: &str) -> Vec<&El> {
        let mut out = Vec::new();
        self.descendants(name, &mut out);
        out
    }
}

fn entity(name: &str) -> Option<String> {
    Some(match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        _ => {
            let n = name.strip_prefix('#')?;
            let code = match n.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => n.parse().ok()?,
            };
            char::from_u32(code)?.to_string()
        }
    })
}

/// Parses `src` into a synthetic root element whose children are the document's top level.
pub(super) fn parse(src: &str) -> Result<El, String> {
    let mut reader = Reader::from_str(src);
    let mut stack: Vec<El> = vec![El::default()];
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| format!("XML error at byte {}: {e}", reader.buffer_position()))?;
        match ev {
            Event::Start(e) => {
                stack.push(element(&e)?);
            }
            Event::Empty(e) => {
                let el = element(&e)?;
                stack.last_mut().expect("root").children.push(el);
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err("unbalanced XML end tag".into());
                }
                let done = stack.pop().expect("element");
                stack.last_mut().expect("root").children.push(done);
            }
            Event::Text(t) => {
                stack
                    .last_mut()
                    .expect("root")
                    .text
                    .push_str(&t.xml10_content());
            }
            Event::CData(t) => {
                stack
                    .last_mut()
                    .expect("root")
                    .text
                    .push_str(&t.xml10_content());
            }
            Event::GeneralRef(r) => {
                let name = r.into_inner();
                if let Some(v) = entity(&name) {
                    stack.last_mut().expect("root").text.push_str(&v);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    // Tolerate unclosed elements at EOF (truncated files): fold them into their parents.
    while stack.len() > 1 {
        let done = stack.pop().expect("element");
        stack.last_mut().expect("root").children.push(done);
    }
    Ok(stack.pop().expect("root"))
}

fn element(e: &quick_xml::events::BytesStart<'_>) -> Result<El, String> {
    let name = e.name().as_ref().to_owned();
    let mut attrs = Vec::new();
    for a in e.attributes().with_checks(false) {
        let a = a.map_err(|e| e.to_string())?;
        let k = a.key.as_ref().to_owned();
        let v = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| a.value.clone().into_owned());
        attrs.push((k, v));
    }
    Ok(El {
        name,
        attrs,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_elements_attributes_text_and_entities() {
        let root = parse(
            r#"<?xml version="1.0"?><a x="1 &amp; 2"><b>t &lt; u</b><c/><b><![CDATA[raw <x>]]></b></a>"#,
        )
        .expect("xml");
        let a = root.child("a").expect("a");
        assert_eq!(a.attr("x"), Some("1 & 2"));
        assert_eq!(a.child_text("b"), Some("t < u"));
        assert_eq!(a.find_all("b").len(), 2);
        assert_eq!(a.find_all("b")[1].text, "raw <x>");
        assert!(a.child("c").is_some());
        assert!(parse("<a><b>").is_ok(), "truncated input is tolerated");
    }
}
