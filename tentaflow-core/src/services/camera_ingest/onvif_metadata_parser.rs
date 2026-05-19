// =============================================================================
// File: services/camera_ingest/onvif_metadata_parser.rs — parser for the
// ONVIF analytics-metadata XML payload carried inside PullPoint
// `NotificationMessage` envelopes (F2 P6.a).
// =============================================================================
//
// ONVIF analytics events are XML fragments under the
// `http://www.onvif.org/ver10/schema` namespace ("tt:"). Each
// `<tt:VideoAnalytics>` block contains one or more `<tt:Frame>` elements,
// and each Frame contains zero or more `<tt:Object>` elements. An Object
// carries:
//   * an `ObjectId` attribute (used as track_id)
//   * a `tt:Appearance` child with `tt:Shape/tt:BoundingBox` (left,top,
//     right,bottom — normalised 0..1 floats) and a
//     `tt:Class/tt:Type` element carrying the class label and an optional
//     `Likelihood` attribute that we map to `confidence`.
//
// Vendor profiles drift in two places:
//   * Axis emits `<tt:Class><tt:Type Likelihood="0.93">Vehicle</tt:Type>`
//   * Hanwha / Bosch emit `<tt:ClassCandidate><tt:Type>Person</tt:Type>
//     <tt:Likelihood>0.85</tt:Likelihood></tt:ClassCandidate>`
//   * Some cameras omit `Class` entirely and only ship the bbox.
//
// The parser is intentionally tolerant: missing class becomes the literal
// "unknown", missing confidence becomes 0.0, missing bbox becomes None. Any
// element we do not recognise is skipped. Malformed numerics are treated as
// missing rather than fatal — a single corrupt event must not stall the
// pull loop.

use crate::services::camera_ingest::onvif_media::{
    extract_open_tag_attr_pub, extract_xml_text_pub, find_close_tag_pub,
};

/// A normalised bounding box. ONVIF emits coordinates in the
/// `tt:Rectangle` schema where left/right are in 0..1 (or the device's
/// declared frame width range) — we forward the values as-is and let the
/// caller decide on coordinate-space normalisation.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundingBox {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// One detected object inside an analytics frame.
#[derive(Debug, Clone, PartialEq)]
pub struct MetadataItem {
    pub class: String,
    pub confidence: f64,
    pub bbox: Option<BoundingBox>,
    pub track_id: Option<String>,
}

/// Parse a metadata XML payload (the `<tt:VideoAnalytics>` block or its
/// enclosing envelope) and return every detected object. Multiple Frames
/// inside the same payload are flattened — the caller does not need to
/// know about frame boundaries because each `MetadataItem` is timestamped
/// by its enclosing `NotificationMessage`, not by the analytics frame.
pub fn parse_metadata_xml(xml: &str) -> Vec<MetadataItem> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some((object_block, object_open_body, end)) = next_object_block(xml, cursor) {
        let track_id = extract_open_tag_attr_pub(object_open_body, "ObjectId");
        let bbox = extract_bounding_box(object_block);
        let (class, confidence) = extract_class_and_confidence(object_block);
        out.push(MetadataItem {
            class,
            confidence,
            bbox,
            track_id,
        });
        cursor = end;
    }
    out
}

/// Walk the XML looking for the next `<...:Object ObjectId="...">...</Object>`
/// element. Returns the inner block, the open-tag body (for attribute
/// access), and the absolute end-of-element offset. Self-closing
/// `<Object .../>` elements carry no payload and are skipped.
fn next_object_block<'a>(xml: &'a str, start: usize) -> Option<(&'a str, &'a str, usize)> {
    let mut cursor = start;
    while cursor < xml.len() {
        let rest = &xml[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/') || after_lt.starts_with('!') || after_lt.starts_with('?') {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == "Object" && !open_body.ends_with('/') {
            let content_start = cursor + lt + 1 + open_end + 1;
            let after_open = &xml[content_start..];
            if let Some(close_idx) = find_close_tag_pub(after_open, "Object") {
                let block = &xml[content_start..content_start + close_idx];
                let end_offset = content_start + close_idx;
                // Step the cursor past the closing `</...:Object>` tag so
                // the next iteration cannot re-match the same block.
                let after_close = &xml[end_offset..];
                let advance = after_close
                    .find('>')
                    .map(|p| end_offset + p + 1)
                    .unwrap_or(end_offset + 1);
                return Some((block, open_body, advance));
            }
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

/// Extract the four bbox edges. ONVIF emits `<tt:BoundingBox>` either with
/// the four edges as attributes on the open tag (Axis, Hanwha) or as four
/// child elements (rare, legacy). We try attributes first, then fall back
/// to children. Missing values disqualify the bbox.
fn extract_bounding_box(block: &str) -> Option<BoundingBox> {
    let (open_body, inner) = find_element_open_and_inner(block, "BoundingBox")?;
    let attr_l = open_body
        .as_deref()
        .and_then(|b| extract_open_tag_attr_pub(b, "left"));
    let attr_t = open_body
        .as_deref()
        .and_then(|b| extract_open_tag_attr_pub(b, "top"));
    let attr_r = open_body
        .as_deref()
        .and_then(|b| extract_open_tag_attr_pub(b, "right"));
    let attr_b = open_body
        .as_deref()
        .and_then(|b| extract_open_tag_attr_pub(b, "bottom"));
    let left = parse_or_child(attr_l.as_deref(), inner.as_deref(), "left")?;
    let top = parse_or_child(attr_t.as_deref(), inner.as_deref(), "top")?;
    let right = parse_or_child(attr_r.as_deref(), inner.as_deref(), "right")?;
    let bottom = parse_or_child(attr_b.as_deref(), inner.as_deref(), "bottom")?;
    Some(BoundingBox {
        left,
        top,
        right,
        bottom,
    })
}

/// Parse a float from either an attribute string or a child element text.
/// Returns None on any parse failure — caller treats that as "missing".
fn parse_or_child(attr: Option<&str>, inner: Option<&str>, child_tag: &str) -> Option<f64> {
    if let Some(v) = attr {
        if let Ok(f) = v.trim().parse::<f64>() {
            return Some(f);
        }
    }
    if let Some(block) = inner {
        if let Some(text) = extract_xml_text_pub(block, child_tag) {
            if let Ok(f) = text.trim().parse::<f64>() {
                return Some(f);
            }
        }
    }
    None
}

/// Find the first occurrence of `tag` and return (open_tag_body, inner_block).
/// `open_tag_body` is None when the element is self-closing (no inner block).
fn find_element_open_and_inner(xml: &str, tag: &str) -> Option<(Option<String>, Option<String>)> {
    let mut cursor = 0usize;
    while cursor < xml.len() {
        let rest = &xml[cursor..];
        let lt = rest.find('<')?;
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with('/') || after_lt.starts_with('!') || after_lt.starts_with('?') {
            cursor += lt + 1;
            continue;
        }
        let open_end = after_lt.find('>')?;
        let open_body = &after_lt[..open_end];
        let name_end = open_body
            .find(|c: char| c.is_ascii_whitespace() || c == '/')
            .unwrap_or(open_body.len());
        let qname = &open_body[..name_end];
        let local = qname.rsplit(':').next().unwrap_or(qname);
        if local == tag {
            if open_body.ends_with('/') {
                return Some((Some(open_body.to_string()), None));
            }
            let content_start = cursor + lt + 1 + open_end + 1;
            let after_open = &xml[content_start..];
            if let Some(close_idx) = find_close_tag_pub(after_open, tag) {
                let inner = after_open[..close_idx].to_string();
                return Some((Some(open_body.to_string()), Some(inner)));
            }
        }
        cursor += lt + 1 + open_end + 1;
    }
    None
}

/// Extract `(class, confidence)`. Tolerates the two common emitter shapes:
///   * `<tt:Class><tt:Type Likelihood="0.93">Vehicle</tt:Type></tt:Class>`
///   * `<tt:ClassCandidate><tt:Type>Person</tt:Type>
///      <tt:Likelihood>0.85</tt:Likelihood></tt:ClassCandidate>`
/// When both shapes are present we prefer the first match in document
/// order. Missing class → "unknown"; missing confidence → 0.0.
fn extract_class_and_confidence(block: &str) -> (String, f64) {
    // Strategy: find a `<...:Type ...>VALUE</Type>` element anywhere in the
    // object block. The value is the class label. The likelihood is either
    // on `Type` as an attribute or in a sibling `<Likelihood>` element.
    let class = extract_xml_text_pub(block, "Type").unwrap_or_else(|| "unknown".to_string());
    let class = if class.is_empty() {
        "unknown".to_string()
    } else {
        class
    };
    // Look for Likelihood as an attribute on the Type open tag.
    let mut confidence = 0.0f64;
    if let Some((Some(type_open), _)) = find_element_open_and_inner(block, "Type") {
        if let Some(lh) = extract_open_tag_attr_pub(&type_open, "Likelihood") {
            if let Ok(f) = lh.trim().parse::<f64>() {
                confidence = f;
            }
        }
    }
    if confidence == 0.0 {
        if let Some(lh) = extract_xml_text_pub(block, "Likelihood") {
            if let Ok(f) = lh.trim().parse::<f64>() {
                confidence = f;
            }
        }
    }
    (class, confidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AXIS_SAMPLE: &str = r#"<tt:MetadataStream xmlns:tt="http://www.onvif.org/ver10/schema">
  <tt:VideoAnalytics>
    <tt:Frame UtcTime="2026-05-17T10:15:30Z">
      <tt:Object ObjectId="42">
        <tt:Appearance>
          <tt:Shape>
            <tt:BoundingBox left="0.10" top="0.20" right="0.40" bottom="0.55"/>
            <tt:CenterOfGravity x="0.25" y="0.375"/>
          </tt:Shape>
          <tt:Class>
            <tt:Type Likelihood="0.93">Vehicle</tt:Type>
          </tt:Class>
        </tt:Appearance>
      </tt:Object>
    </tt:Frame>
  </tt:VideoAnalytics>
</tt:MetadataStream>"#;

    const HANWHA_SAMPLE: &str = r#"<tt:MetadataStream xmlns:tt="http://www.onvif.org/ver10/schema">
  <tt:VideoAnalytics>
    <tt:Frame UtcTime="2026-05-17T10:15:31Z">
      <tt:Object ObjectId="7">
        <tt:Appearance>
          <tt:Shape>
            <tt:BoundingBox>
              <tt:left>0.05</tt:left>
              <tt:top>0.10</tt:top>
              <tt:right>0.30</tt:right>
              <tt:bottom>0.45</tt:bottom>
            </tt:BoundingBox>
          </tt:Shape>
          <tt:ClassCandidate>
            <tt:Type>Person</tt:Type>
            <tt:Likelihood>0.85</tt:Likelihood>
          </tt:ClassCandidate>
        </tt:Appearance>
      </tt:Object>
      <tt:Object ObjectId="8">
        <tt:Appearance>
          <tt:Shape>
            <tt:BoundingBox left="0.50" top="0.20" right="0.80" bottom="0.60"/>
          </tt:Shape>
        </tt:Appearance>
      </tt:Object>
    </tt:Frame>
  </tt:VideoAnalytics>
</tt:MetadataStream>"#;

    #[test]
    fn parses_axis_style_attributes_with_likelihood_attr() {
        let items = parse_metadata_xml(AXIS_SAMPLE);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].class, "Vehicle");
        assert!((items[0].confidence - 0.93).abs() < 1e-9);
        assert_eq!(items[0].track_id.as_deref(), Some("42"));
        let bb = items[0].bbox.as_ref().expect("bbox");
        assert!((bb.left - 0.10).abs() < 1e-9);
        assert!((bb.right - 0.40).abs() < 1e-9);
    }

    #[test]
    fn parses_hanwha_style_children_with_class_candidate() {
        let items = parse_metadata_xml(HANWHA_SAMPLE);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].class, "Person");
        assert!((items[0].confidence - 0.85).abs() < 1e-9);
        let bb = items[0].bbox.as_ref().expect("bbox");
        assert!((bb.top - 0.10).abs() < 1e-9);
        assert!((bb.bottom - 0.45).abs() < 1e-9);
        // Second object has bbox but no class → falls back to "unknown".
        assert_eq!(items[1].class, "unknown");
        assert!(items[1].bbox.is_some());
        assert_eq!(items[1].confidence, 0.0);
    }

    #[test]
    fn empty_input_yields_no_items() {
        assert!(parse_metadata_xml("").is_empty());
        assert!(parse_metadata_xml("<tt:MetadataStream/>").is_empty());
    }

    #[test]
    fn malformed_xml_is_tolerated_no_panic() {
        // Truncated payload — parser must not panic, just return what it
        // can extract (likely empty).
        let truncated =
            "<tt:Object ObjectId=\"1\"><tt:Appearance><tt:Shape><tt:BoundingBox left=\"0.1\"";
        let _ = parse_metadata_xml(truncated);
        // Random garbage.
        let garbage = "<<<><<<><><><";
        let _ = parse_metadata_xml(garbage);
    }

    #[test]
    fn unknown_tags_are_ignored() {
        // Vendor adds proprietary children — parser must not choke.
        let xml = r#"<tt:VideoAnalytics xmlns:tt="http://www.onvif.org/ver10/schema">
          <tt:Object ObjectId="99">
            <vendor:Proprietary>data</vendor:Proprietary>
            <tt:Appearance>
              <tt:Shape>
                <tt:BoundingBox left="0.0" top="0.0" right="1.0" bottom="1.0"/>
              </tt:Shape>
              <tt:Class><tt:Type Likelihood="0.5">Animal</tt:Type></tt:Class>
            </tt:Appearance>
            <vendor:Extra>more</vendor:Extra>
          </tt:Object>
        </tt:VideoAnalytics>"#;
        let items = parse_metadata_xml(xml);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].class, "Animal");
        assert_eq!(items[0].track_id.as_deref(), Some("99"));
    }
}
