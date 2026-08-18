//! Pure-Rust PowerPoint engine boundary.
//!
//! The `pptx` crate owns high-level presentation and shape parsing. AiTUI keeps
//! its own JSON contract and will continue to use its guarded OPC editor for
//! exact package/XML operations that cannot safely round-trip through a typed
//! model.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use pptx::opc::{part_type_from_content_type, PartType};
use pptx::{Presentation, Shape, ShapeTree};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{json, Value};

const PRESENTATION_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
const IMAGE_RELATIONSHIP_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const EMU_PER_INCH: f64 = 914_400.0;
const SLIDE_WIDTH_INCHES: f64 = 13.333_333;
const SLIDE_HEIGHT_INCHES: f64 = 7.5;
const DEFAULT_SAFE_MARGIN_INCHES: f64 = 0.375;
const CONTINUITY_TOLERANCE_INCHES: f64 = 0.08;

#[derive(Clone, Copy, Debug)]
struct ElementBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl ElementBounds {
    fn right(self) -> f64 {
        self.x + self.width
    }

    fn bottom(self) -> f64 {
        self.y + self.height
    }

    fn contains(self, other: Self) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }

    fn overlaps(self, other: Self) -> bool {
        self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }
}

/// Create a presentation from the specialized tool's JSON contract.
///
/// Static content is built with the Rust `pptx` crate. The deliberately small
/// animation and transition set is emitted as fixed OOXML templates, then the
/// complete package is serialized, reopened, structurally checked, and only
/// then atomically installed at `output`.
pub fn create(
    spec: &serde_json::Map<String, Value>,
    output: &Path,
    cwd: &Path,
) -> Result<Value, String> {
    let slides = spec
        .get("slides")
        .and_then(Value::as_array)
        .ok_or("slides must be an array")?;
    let diagnostics = validate_deck_design(spec, slides)?;
    let mut presentation = Presentation::new().map_err(|error| error.to_string())?;
    presentation
        .set_slide_width((SLIDE_WIDTH_INCHES * EMU_PER_INCH) as i64)
        .map_err(|error| error.to_string())?;
    presentation
        .set_slide_height((SLIDE_HEIGHT_INCHES * EMU_PER_INCH) as i64)
        .map_err(|error| error.to_string())?;
    let layouts = presentation
        .slide_layouts()
        .map_err(|error| error.to_string())?;
    let layout = layouts
        .iter()
        .find(|layout| layout.name.eq_ignore_ascii_case("blank"))
        .or_else(|| layouts.get(6))
        .or_else(|| layouts.first())
        .ok_or("default presentation template has no slide layout")?
        .clone();

    for (slide_index, slide) in slides.iter().enumerate() {
        let slide = slide
            .as_object()
            .ok_or_else(|| format!("slide {} must be an object", slide_index + 1))?;
        let slide_ref = presentation
            .add_slide(&layout)
            .map_err(|error| error.to_string())?;
        let elements = slide
            .get("elements")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut xml = presentation
            .slide_xml(&slide_ref)
            .map_err(|error| error.to_string())?
            .to_vec();
        let mut shape_ids = HashMap::new();
        for (element_index, element) in elements.iter().enumerate() {
            let element = element.as_object().ok_or_else(|| {
                format!(
                    "slide {} element {} must be an object",
                    slide_index + 1,
                    element_index + 1
                )
            })?;
            let id = required_string(element, "id")?;
            if shape_ids.contains_key(id) {
                return Err(format!(
                    "duplicate element ID on slide {}: {id}",
                    slide_index + 1
                ));
            }
            let shape_id = u32::try_from(element_index + 2)
                .map_err(|_| "too many elements on slide".to_string())?;
            let fragment = element_xml(&mut presentation, &slide_ref, element, shape_id, cwd)?;
            xml = insert_before(&xml, b"</p:spTree>", fragment.as_bytes())?;
            shape_ids.insert(id.to_string(), shape_id);
        }
        let effects = slide_effects_xml(slide, &shape_ids, slide_index + 1)?;
        if !effects.is_empty() {
            xml = insert_slide_effects(&xml, effects.as_bytes())?;
        }
        *presentation
            .slide_xml_mut(&slide_ref)
            .map_err(|error| error.to_string())? = xml;
    }

    let bytes = presentation.to_bytes().map_err(|error| error.to_string())?;
    let reopened = Presentation::from_bytes(&bytes)
        .map_err(|error| format!("generated package could not be reopened: {error}"))?;
    let actual_slides = reopened.slide_count().map_err(|error| error.to_string())?;
    if actual_slides != slides.len() {
        return Err(format!(
            "slide count mismatch after serialization: expected {}, got {actual_slides}",
            slides.len()
        ));
    }
    for (index, slide_ref) in reopened
        .slides()
        .map_err(|error| error.to_string())?
        .iter()
        .enumerate()
    {
        let tree = ShapeTree::from_slide_xml(
            reopened
                .slide_xml(slide_ref)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let expected = slides[index]
            .get("elements")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if tree.len() != expected {
            return Err(format!(
                "slide {} shape count mismatch after serialization: expected {expected}, got {}",
                index + 1,
                tree.len()
            ));
        }
    }
    atomic_write(output, &bytes)?;
    Ok(json!({
        "engine": "aitui-native-rust",
        "operation": "create",
        "path": output,
        "slides": actual_slides,
        "external_runtime_required": false,
        "design_validation": diagnostics,
    }))
}

pub fn append(
    spec: &serde_json::Map<String, Value>,
    input: &Path,
    output: &Path,
    cwd: &Path,
) -> Result<Value, String> {
    let slides = spec
        .get("slides")
        .and_then(Value::as_array)
        .ok_or("slides must be an array")?;
    validate_deck_design(spec, slides)?;
    let mut presentation = Presentation::open(input).map_err(|error| error.to_string())?;
    let layouts = presentation
        .slide_layouts()
        .map_err(|error| error.to_string())?;
    let layout = layouts
        .iter()
        .find(|layout| layout.name.eq_ignore_ascii_case("blank"))
        .or_else(|| layouts.get(6))
        .or_else(|| layouts.first())
        .ok_or("presentation has no slide layout")?
        .clone();
    for (index, slide) in slides.iter().enumerate() {
        add_slide_from_value(&mut presentation, &layout, slide, index + 1, cwd)?;
    }
    save_changed_presentation(presentation, output, "append")
}

pub fn edit(
    spec: &serde_json::Map<String, Value>,
    input: &Path,
    output: &Path,
) -> Result<Value, String> {
    if spec
        .get("package_modifiers")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        return Err("package_modifiers are not yet supported by the native editor".into());
    }
    let modifiers = spec
        .get("modifiers")
        .and_then(Value::as_array)
        .ok_or("modifiers must be an array")?;
    let mut presentation = Presentation::open(input).map_err(|error| error.to_string())?;
    for modifier in modifiers {
        let modifier = modifier
            .as_object()
            .ok_or("PowerPoint modifiers must be objects")?;
        match required_string(modifier, "operation")? {
            "update_element" => {
                let slide_index = json_index(modifier, "slide_index")?;
                let element_id = required_string(modifier, "element_id")?;
                let changes = modifier
                    .get("changes")
                    .and_then(Value::as_object)
                    .ok_or("update_element.changes must be an object")?;
                let slide_ref = presentation
                    .slides_get(slide_index)
                    .map_err(|error| error.to_string())?;
                let xml = presentation
                    .slide_xml(&slide_ref)
                    .map_err(|error| error.to_string())?;
                let updated = update_named_shape_xml(xml, element_id, changes)?;
                *presentation
                    .slide_xml_mut(&slide_ref)
                    .map_err(|error| error.to_string())? = updated;
            }
            "set_transition" => {
                let slide_index = json_index(modifier, "slide_index")?;
                let slide_ref = presentation
                    .slides_get(slide_index)
                    .map_err(|error| error.to_string())?;
                let xml = presentation
                    .slide_xml(&slide_ref)
                    .map_err(|error| error.to_string())?;
                let updated = set_transition_xml(xml, modifier.get("transition"))?;
                *presentation
                    .slide_xml_mut(&slide_ref)
                    .map_err(|error| error.to_string())? = updated;
            }
            operation => {
                return Err(format!(
                    "native editor does not yet support modifier operation: {operation}"
                ))
            }
        }
    }
    save_changed_presentation(presentation, output, "edit")
}

fn add_slide_from_value(
    presentation: &mut Presentation,
    layout: &pptx::slide::SlideLayoutRef,
    slide: &Value,
    slide_number: usize,
    cwd: &Path,
) -> Result<(), String> {
    let slide = slide
        .as_object()
        .ok_or_else(|| format!("slide {slide_number} must be an object"))?;
    let slide_ref = presentation
        .add_slide(layout)
        .map_err(|error| error.to_string())?;
    let elements = slide
        .get("elements")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut xml = presentation
        .slide_xml(&slide_ref)
        .map_err(|error| error.to_string())?
        .to_vec();
    let mut shape_ids = HashMap::new();
    for (element_index, element) in elements.iter().enumerate() {
        let element = element
            .as_object()
            .ok_or_else(|| format!("slide {slide_number} element must be an object"))?;
        let id = required_string(element, "id")?;
        if shape_ids.contains_key(id) {
            return Err(format!(
                "duplicate element ID on slide {slide_number}: {id}"
            ));
        }
        let shape_id = u32::try_from(element_index + 2)
            .map_err(|_| "too many elements on slide".to_string())?;
        let fragment = element_xml(presentation, &slide_ref, element, shape_id, cwd)?;
        xml = insert_before(&xml, b"</p:spTree>", fragment.as_bytes())?;
        shape_ids.insert(id.to_string(), shape_id);
    }
    let effects = slide_effects_xml(slide, &shape_ids, slide_number)?;
    if !effects.is_empty() {
        xml = insert_slide_effects(&xml, effects.as_bytes())?;
    }
    *presentation
        .slide_xml_mut(&slide_ref)
        .map_err(|error| error.to_string())? = xml;
    Ok(())
}

fn save_changed_presentation(
    presentation: Presentation,
    output: &Path,
    operation: &str,
) -> Result<Value, String> {
    let bytes = presentation.to_bytes().map_err(|error| error.to_string())?;
    let reopened = Presentation::from_bytes(&bytes)
        .map_err(|error| format!("serialized package could not be reopened: {error}"))?;
    let slides = reopened.slide_count().map_err(|error| error.to_string())?;
    atomic_write(output, &bytes)?;
    Ok(json!({
        "engine": "aitui-native-rust",
        "operation": operation,
        "path": output,
        "slides": slides,
    }))
}

fn update_named_shape_xml(
    xml: &[u8],
    element_id: &str,
    changes: &serde_json::Map<String, Value>,
) -> Result<Vec<u8>, String> {
    let source = String::from_utf8(xml.to_vec())
        .map_err(|error| format!("slide XML is not UTF-8: {error}"))?;
    let marker = format!("name=\"{}\"", xml_escape(element_id));
    let marker_position = source
        .find(&marker)
        .ok_or_else(|| format!("element {element_id:?} does not exist on slide"))?;
    let shape_start = source[..marker_position]
        .rfind("<p:sp>")
        .ok_or("selected element is not an editable shape")?;
    let shape_end = source[marker_position..]
        .find("</p:sp>")
        .map(|offset| marker_position + offset + "</p:sp>".len())
        .ok_or("selected shape XML is incomplete")?;
    let mut shape = source[shape_start..shape_end].to_string();
    if let Some(text) = changes.get("text") {
        let text = text.as_str().ok_or("changes.text must be a string")?;
        let start = shape
            .find("<a:t>")
            .ok_or("element has no editable text run")?;
        let end = shape[start..]
            .find("</a:t>")
            .map(|offset| start + offset)
            .ok_or("element has no complete editable text run")?;
        shape.replace_range(start + "<a:t>".len()..end, &xml_escape(text));
    }
    let mut updated = String::with_capacity(source.len() - (shape_end - shape_start) + shape.len());
    updated.push_str(&source[..shape_start]);
    updated.push_str(&shape);
    updated.push_str(&source[shape_end..]);
    Ok(updated.into_bytes())
}

fn set_transition_xml(xml: &[u8], transition: Option<&Value>) -> Result<Vec<u8>, String> {
    let mut source = String::from_utf8(xml.to_vec())
        .map_err(|error| format!("slide XML is not UTF-8: {error}"))?;
    if let Some(start) = source.find("<p:transition") {
        let end = source[start..]
            .find("</p:transition>")
            .map(|offset| start + offset + "</p:transition>".len())
            .or_else(|| source[start..].find("/>").map(|offset| start + offset + 2))
            .ok_or("existing transition XML is incomplete")?;
        source.replace_range(start..end, "");
    }
    let Some(transition) = transition.filter(|value| !value.is_null()) else {
        return Ok(source.into_bytes());
    };
    let child = match transition
        .as_str()
        .ok_or("transition must be a string or null")?
    {
        "fade" => "<p:fade/>",
        "push_left" => "<p:push dir=\"l\"/>",
        "wipe_left" => "<p:wipe dir=\"l\"/>",
        value => return Err(format!("unsupported slide transition: {value}")),
    };
    let fragment = format!("<p:transition spd=\"med\">{child}</p:transition>");
    if let Some(position) = source.find("<p:timing") {
        source.insert_str(position, &fragment);
    } else if let Some(position) = source.find("<p:extLst") {
        source.insert_str(position, &fragment);
    } else {
        let position = source
            .rfind("</p:sld>")
            .ok_or("slide XML has no closing p:sld")?;
        source.insert_str(position, &fragment);
    }
    Ok(source.into_bytes())
}

fn validate_deck_design(
    spec: &serde_json::Map<String, Value>,
    slides: &[Value],
) -> Result<Value, String> {
    let design = spec.get("design").and_then(Value::as_object);
    let overlap_policy = design
        .and_then(|value| value.get("overlap_policy"))
        .and_then(Value::as_str)
        .unwrap_or("error");
    let continuity_policy = design
        .and_then(|value| value.get("continuity_policy"))
        .and_then(Value::as_str)
        .unwrap_or("warn");
    if !matches!(overlap_policy, "error" | "warn" | "allow") {
        return Err("design.overlap_policy must be error, warn, or allow".into());
    }
    if !matches!(continuity_policy, "error" | "warn" | "off") {
        return Err("design.continuity_policy must be error, warn, or off".into());
    }
    let safe_margin = design
        .and_then(|value| value.get("safe_margin"))
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_SAFE_MARGIN_INCHES);
    if !safe_margin.is_finite() || !(0.0..=1.5).contains(&safe_margin) {
        return Err("design.safe_margin must be between 0 and 1.5 inches".into());
    }

    let mut warnings = Vec::new();
    let mut continuity: HashMap<String, HashMap<String, ElementBounds>> = HashMap::new();
    for (slide_index, slide) in slides.iter().enumerate() {
        let slide = slide
            .as_object()
            .ok_or_else(|| format!("slide {} must be an object", slide_index + 1))?;
        let elements = slide
            .get("elements")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut geometry = Vec::with_capacity(elements.len());
        for (element_index, element) in elements.iter().enumerate() {
            let element = element.as_object().ok_or_else(|| {
                format!(
                    "slide {} element {} must be an object",
                    slide_index + 1,
                    element_index + 1
                )
            })?;
            let id = required_string(element, "id")?;
            let bounds = ElementBounds {
                x: numeric_inches(element, "x", false)?,
                y: numeric_inches(element, "y", false)?,
                width: numeric_inches(element, "width", true)?,
                height: numeric_inches(element, "height", true)?,
            };
            if bounds.right() > SLIDE_WIDTH_INCHES + 0.001
                || bounds.bottom() > SLIDE_HEIGHT_INCHES + 0.001
            {
                return Err(format!(
                    "slide {} element {id:?} extends outside the 13.333×7.5 inch slide",
                    slide_index + 1
                ));
            }
            let kind = required_string(element, "type")?;
            let is_background = kind == "shape"
                && bounds.x <= 0.01
                && bounds.y <= 0.01
                && bounds.right() >= SLIDE_WIDTH_INCHES - 0.02
                && bounds.bottom() >= SLIDE_HEIGHT_INCHES - 0.02;
            if !is_background
                && (bounds.x < safe_margin
                    || bounds.y < safe_margin
                    || bounds.right() > SLIDE_WIDTH_INCHES - safe_margin
                    || bounds.bottom() > SLIDE_HEIGHT_INCHES - safe_margin)
            {
                warnings.push(json!({
                    "code": "SAFE_AREA",
                    "slide": slide_index + 1,
                    "element": id,
                    "message": format!("element is inside the {safe_margin:.2} inch compatibility safe area")
                }));
            }
            if kind == "text" {
                if let Some(text) = element.get("text").and_then(Value::as_str) {
                    let font_size = element
                        .get("font_size")
                        .and_then(Value::as_f64)
                        .unwrap_or(24.0);
                    let estimated_lines = estimate_text_lines(text, bounds.width, font_size);
                    let line_capacity = ((bounds.height * 72.0) / (font_size * 1.22))
                        .floor()
                        .max(1.0) as usize;
                    if estimated_lines > line_capacity {
                        let message = format!(
                            "slide {} text element {id:?} may wrap to {estimated_lines} lines but its box safely fits about {line_capacity}; enlarge it or reduce text",
                            slide_index + 1
                        );
                        if overlap_policy == "error" {
                            return Err(message);
                        }
                        warnings.push(json!({"code": "TEXT_FIT", "slide": slide_index + 1, "element": id, "message": message}));
                    }
                }
            }
            geometry.push((
                id,
                kind,
                bounds,
                element
                    .get("allow_overlap")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ));
        }

        if overlap_policy != "allow" {
            for left_index in 0..geometry.len() {
                for right_index in left_index + 1..geometry.len() {
                    let (left_id, left_kind, left, left_allowed) = geometry[left_index];
                    let (right_id, _, right, right_allowed) = geometry[right_index];
                    if !left.overlaps(right) || left_allowed || right_allowed {
                        continue;
                    }
                    // A later element fully contained by an earlier shape is the normal
                    // card/background + content pattern, not an accidental collision.
                    if left_kind == "shape" && left.contains(right) {
                        continue;
                    }
                    let message = format!(
                        "slide {} elements {left_id:?} and {right_id:?} overlap; set allow_overlap only for an intentional composition",
                        slide_index + 1
                    );
                    if overlap_policy == "error" {
                        return Err(message);
                    }
                    warnings.push(json!({"code": "OVERLAP", "slide": slide_index + 1, "elements": [left_id, right_id], "message": message}));
                }
            }
        }

        if let Some(group) = slide.get("continuity_group").and_then(Value::as_str) {
            let current = geometry
                .iter()
                .map(|(id, _, bounds, _)| ((*id).to_string(), *bounds))
                .collect::<HashMap<_, _>>();
            if continuity_policy != "off" {
                if let Some(previous) = continuity.get(group) {
                    for (id, bounds) in &current {
                        let Some(prior) = previous.get(id) else {
                            continue;
                        };
                        let delta = (bounds.x - prior.x).abs()
                            + (bounds.y - prior.y).abs()
                            + (bounds.width - prior.width).abs()
                            + (bounds.height - prior.height).abs();
                        if delta > CONTINUITY_TOLERANCE_INCHES {
                            let message = format!(
                                "slide {} continuity element {id:?} moved or resized within group {group:?}; keep persistent anchors fixed",
                                slide_index + 1
                            );
                            if continuity_policy == "error" {
                                return Err(message);
                            }
                            warnings.push(json!({"code": "CONTINUITY", "slide": slide_index + 1, "element": id, "group": group, "message": message}));
                        }
                    }
                }
            }
            continuity.insert(group.to_string(), current);
        }
    }
    Ok(json!({
        "profile": design.and_then(|value| value.get("profile")).and_then(Value::as_str).unwrap_or("libreoffice_safe"),
        "overlap_policy": overlap_policy,
        "continuity_policy": continuity_policy,
        "safe_margin": safe_margin,
        "warnings": warnings,
    }))
}

fn numeric_inches(
    object: &serde_json::Map<String, Value>,
    field: &str,
    positive: bool,
) -> Result<f64, String> {
    let value = object
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{field} must be a number"))?;
    if !value.is_finite() || (positive && value <= 0.0) || (!positive && value < 0.0) {
        return Err(format!("{field} has an invalid inch value"));
    }
    Ok(value)
}

fn estimate_text_lines(text: &str, width_inches: f64, font_size: f64) -> usize {
    let characters_per_line = ((width_inches * 72.0) / (font_size * 0.52))
        .floor()
        .max(1.0) as usize;
    text.lines()
        .map(|line| line.chars().count().max(1).div_ceil(characters_per_line))
        .sum::<usize>()
        .max(1)
}

fn text_xml(text: &str, font_size: i64, text_color: &str) -> String {
    text.split('\n')
        .map(|line| format!(
            "<a:p><a:r><a:rPr lang=\"en-US\" sz=\"{font_size}\" dirty=\"0\"><a:solidFill><a:srgbClr val=\"{text_color}\"/></a:solidFill><a:latin typeface=\"Liberation Sans\"/></a:rPr><a:t>{}</a:t></a:r><a:endParaRPr lang=\"en-US\" sz=\"{font_size}\"><a:latin typeface=\"Liberation Sans\"/></a:endParaRPr></a:p>",
            xml_escape(line)
        ))
        .collect::<String>()
}

fn json_index(object: &serde_json::Map<String, Value>, field: &str) -> Result<usize, String> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("{field} must be a non-negative integer"))
}

fn element_xml(
    presentation: &mut Presentation,
    slide_ref: &pptx::slide::SlideRef,
    element: &serde_json::Map<String, Value>,
    shape_id: u32,
    cwd: &Path,
) -> Result<String, String> {
    let id = required_string(element, "id")?;
    let kind = required_string(element, "type")?;
    let x = inches(element, "x", false)?;
    let y = inches(element, "y", false)?;
    let width = inches(element, "width", true)?;
    let height = inches(element, "height", true)?;
    match kind {
        "text" => {
            let text = element
                .get("text")
                .and_then(Value::as_str)
                .ok_or("text elements require a string 'text'")?;
            Ok(shape_xml(
                element,
                shape_id,
                id,
                (x, y, width, height),
                text,
                true,
            ))
        }
        "shape" => Ok(shape_xml(
            element,
            shape_id,
            id,
            (x, y, width, height),
            element.get("text").and_then(Value::as_str).unwrap_or(""),
            false,
        )),
        "image" => {
            let raw = element
                .get("image_path")
                .and_then(Value::as_str)
                .ok_or("image elements require 'image_path'")?;
            let source = {
                let path = PathBuf::from(raw);
                if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }
            };
            let data = fs::read(&source)
                .map_err(|error| format!("cannot read image {}: {error}", source.display()))?;
            let extension = source
                .extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .ok_or_else(|| format!("image has no extension: {}", source.display()))?;
            let content_type = match extension.as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => return Err(format!("unsupported image type: {extension}")),
            };
            let partname = presentation
                .package()
                .next_image_partname(&extension)
                .map_err(|error| error.to_string())?;
            let target = partname.relative_ref(slide_ref.partname.base_uri());
            presentation
                .package_mut()
                .put_part(pptx::opc::Part::new(partname, content_type, data));
            let slide_part = presentation
                .package_mut()
                .part_mut(&slide_ref.partname)
                .ok_or_else(|| format!("missing slide part {}", slide_ref.partname))?;
            let relationship_id =
                slide_part
                    .rels
                    .add_relationship(IMAGE_RELATIONSHIP_TYPE, target, false);
            Ok(picture_xml(
                shape_id,
                id,
                &relationship_id,
                x,
                y,
                width,
                height,
            ))
        }
        _ => Err(format!("unsupported element type: {kind}")),
    }
}

fn shape_xml(
    element: &serde_json::Map<String, Value>,
    shape_id: u32,
    id: &str,
    bounds: (i64, i64, i64, i64),
    text: &str,
    textbox: bool,
) -> String {
    let (x, y, width, height) = bounds;
    let geometry = if textbox {
        "rect"
    } else {
        match element
            .get("shape_type")
            .and_then(Value::as_str)
            .unwrap_or("rectangle")
        {
            "ellipse" => "ellipse",
            "rounded_rectangle" => "roundRect",
            _ => "rect",
        }
    };
    let fill = color(element, "fill_color", "4472C4");
    let text_color = color(
        element,
        "text_color",
        if textbox { "1F2937" } else { "FFFFFF" },
    );
    let font_size = element
        .get("font_size")
        .and_then(Value::as_f64)
        .filter(|value| *value > 0.0)
        .unwrap_or(24.0);
    let font_size = (font_size * 100.0).round() as i64;
    let textbox_attribute = if textbox { " txBox=\"1\"" } else { "" };
    let shape_style = if textbox {
        "<a:noFill/><a:ln><a:noFill/></a:ln>".to_string()
    } else {
        format!("<a:solidFill><a:srgbClr val=\"{fill}\"/></a:solidFill><a:ln><a:solidFill><a:srgbClr val=\"{fill}\"/></a:solidFill></a:ln>")
    };
    let paragraphs = text_xml(text, font_size, &text_color);
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{shape_id}\" name=\"{}\"/><p:cNvSpPr{textbox_attribute}/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{width}\" cy=\"{height}\"/></a:xfrm><a:prstGeom prst=\"{geometry}\"><a:avLst/></a:prstGeom>{shape_style}</p:spPr><p:txBody><a:bodyPr wrap=\"square\" lIns=\"45720\" rIns=\"45720\" tIns=\"22860\" bIns=\"22860\" anchor=\"t\"/><a:lstStyle/>{paragraphs}</p:txBody></p:sp>",
        xml_escape(id),
    )
}

fn picture_xml(
    shape_id: u32,
    id: &str,
    relationship_id: &str,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
) -> String {
    format!(
        "<p:pic><p:nvPicPr><p:cNvPr id=\"{shape_id}\" name=\"{}\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"{}\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{width}\" cy=\"{height}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>",
        xml_escape(id),
        xml_escape(relationship_id),
    )
}

fn slide_effects_xml(
    slide: &serde_json::Map<String, Value>,
    shape_ids: &HashMap<String, u32>,
    slide_number: usize,
) -> Result<String, String> {
    let mut result = String::new();
    if let Some(transition) = slide.get("transition") {
        if !transition.is_null() {
            let transition = transition
                .as_str()
                .ok_or("slide transition must be a string or null")?;
            let child = match transition {
                "fade" => "<p:fade/>",
                "push_left" => "<p:push dir=\"l\"/>",
                "wipe_left" => "<p:wipe dir=\"l\"/>",
                _ => return Err(format!("unsupported slide transition: {transition}")),
            };
            result.push_str(&format!("<p:transition spd=\"med\">{child}</p:transition>"));
        }
    }
    let animations = slide
        .get("animations")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let animation_mode = slide
        .get("animation_mode")
        .and_then(Value::as_str)
        .unwrap_or("single_click");
    if animation_mode == "none" {
        return Ok(result);
    }
    if !matches!(animation_mode, "single_click" | "explicit") {
        return Err(format!("unsupported animation_mode: {animation_mode}"));
    }
    if animations.is_empty() {
        return Ok(result);
    }
    let mut ordered = animations.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|animation| animation.get("order").and_then(Value::as_u64).unwrap_or(0));
    let mut effects = String::new();
    let mut node_id = 3u64;
    for (animation_index, animation) in ordered.into_iter().enumerate() {
        let animation = animation
            .as_object()
            .ok_or("animations must contain objects")?;
        let target = required_string(animation, "target")?;
        let shape_id = shape_ids.get(target).ok_or_else(|| {
            format!("slide {slide_number} animation target {target:?} does not exist")
        })?;
        let kind = required_string(animation, "type")?;
        let (class, preset, subtype, filter, direction) = match kind {
            "fade_in" => ("entr", "10", "0", "fade", "in"),
            "fly_in_left" => ("entr", "2", "8", "slide(fromLeft)", "in"),
            "fly_in_right" => ("entr", "2", "2", "slide(fromRight)", "in"),
            "fly_in_bottom" => ("entr", "2", "4", "slide(fromBottom)", "in"),
            "wipe" => ("entr", "22", "1", "wipe(right)", "in"),
            "zoom" => ("entr", "23", "0", "zoom(in)", "in"),
            "fade_out" => ("exit", "10", "0", "fade", "out"),
            _ => return Err(format!("unsupported animation type: {kind}")),
        };
        let duration = animation
            .get("duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or(500);
        if !(1..=60_000).contains(&duration) {
            return Err("animation duration_ms must be between 1 and 60000".into());
        }
        let delay = animation
            .get("delay_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if delay > 60_000 {
            return Err("animation delay_ms must be between 0 and 60000".into());
        }
        let trigger = if animation_mode == "single_click" {
            if animation_index == 0 {
                "on_click"
            } else {
                "after_previous"
            }
        } else {
            animation
                .get("trigger")
                .and_then(Value::as_str)
                .unwrap_or("after_previous")
        };
        let (node_type, start_delay) = match trigger {
            "on_click" => ("clickEffect", "indefinite".to_string()),
            "with_previous" => ("withEffect", delay.to_string()),
            "after_previous" => ("afterEffect", delay.to_string()),
            _ => return Err(format!("unsupported animation trigger: {trigger}")),
        };
        effects.push_str(&format!(
            "<p:par><p:cTn id=\"{node_id}\" fill=\"hold\" nodeType=\"{node_type}\" presetClass=\"{class}\" presetID=\"{preset}\" presetSubtype=\"{subtype}\"><p:stCondLst><p:cond delay=\"{start_delay}\"/></p:stCondLst><p:childTnLst><p:animEffect transition=\"{direction}\" filter=\"{filter}\"><p:cBhvr><p:cTn id=\"{}\" dur=\"{duration}\" fill=\"hold\"/><p:tgtEl><p:spTgt spid=\"{shape_id}\"/></p:tgtEl></p:cBhvr></p:animEffect></p:childTnLst></p:cTn></p:par>",
            node_id + 1
        ));
        node_id += 2;
    }
    result.push_str(&format!(
        "<p:timing><p:tnLst><p:par><p:cTn id=\"1\" dur=\"indefinite\" restart=\"never\" nodeType=\"tmRoot\"><p:childTnLst><p:seq concurrent=\"1\" nextAc=\"seek\"><p:cTn id=\"2\" dur=\"indefinite\" nodeType=\"mainSeq\"><p:childTnLst>{effects}</p:childTnLst></p:cTn><p:prevCondLst><p:cond evt=\"onPrev\" delay=\"0\"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:prevCondLst><p:nextCondLst><p:cond evt=\"onNext\" delay=\"0\"><p:tgtEl><p:sldTgt/></p:tgtEl></p:cond></p:nextCondLst></p:seq></p:childTnLst></p:cTn></p:par></p:tnLst></p:timing>"
    ));
    Ok(result)
}

fn insert_slide_effects(xml: &[u8], effects: &[u8]) -> Result<Vec<u8>, String> {
    if find_bytes(xml, b"<p:extLst").is_some() {
        insert_before(xml, b"<p:extLst", effects)
    } else {
        insert_before(xml, b"</p:sld>", effects)
    }
}

fn insert_before(xml: &[u8], marker: &[u8], fragment: &[u8]) -> Result<Vec<u8>, String> {
    let position = find_bytes(xml, marker).ok_or_else(|| {
        format!(
            "generated slide XML is missing {}",
            String::from_utf8_lossy(marker)
        )
    })?;
    let mut result = Vec::with_capacity(xml.len() + fragment.len());
    result.extend_from_slice(&xml[..position]);
    result.extend_from_slice(fragment);
    result.extend_from_slice(&xml[position..]);
    Ok(result)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

fn inches(
    object: &serde_json::Map<String, Value>,
    field: &str,
    positive: bool,
) -> Result<i64, String> {
    let value = object
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{field} must be a number"))?;
    if !value.is_finite() || (positive && value <= 0.0) || (!positive && value < 0.0) {
        return Err(format!(
            "{field} must be {}",
            if positive {
                "greater than zero"
            } else {
                "non-negative"
            }
        ));
    }
    Ok((value * EMU_PER_INCH).round() as i64)
}

fn color(object: &serde_json::Map<String, Value>, field: &str, default: &str) -> String {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or(default)
        .to_ascii_uppercase()
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn atomic_write(output: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let temporary = temporary_path(output);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", temporary.display()))?;
        fs::rename(&temporary, output)
            .map_err(|error| format!("cannot replace {}: {error}", output.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Inspect an imported deck without writing it.
///
/// This is intentionally read-only: opening and parsing the package must never
/// mutate the source file. The returned selectors use native OOXML IDs rather
/// than positional indices or potentially ambiguous shape names.
pub fn inspect(path: &Path) -> Result<Value, String> {
    let presentation = Presentation::open(path).map_err(|error| error.to_string())?;
    let slide_ids = parse_slide_ids(&presentation)?;
    let slide_refs = presentation.slides().map_err(|error| error.to_string())?;
    let slide_size = presentation
        .slide_size()
        .map_err(|error| error.to_string())?;

    let mut slides = Vec::with_capacity(slide_refs.len());
    for (index, slide_ref) in slide_refs.iter().enumerate() {
        let slide_id = slide_ids
            .get(slide_ref.r_id.as_str())
            .copied()
            .ok_or_else(|| {
                format!(
                    "slide relationship {} has no native slide ID",
                    slide_ref.r_id
                )
            })?;
        let xml = presentation
            .slide_xml(slide_ref)
            .map_err(|error| error.to_string())?;
        let tree = ShapeTree::from_slide_xml(xml).map_err(|error| error.to_string())?;
        let shapes = tree
            .iter()
            .map(|shape| inspect_shape(slide_id, shape))
            .collect::<Vec<_>>();
        let part = presentation
            .package()
            .part(&slide_ref.partname)
            .ok_or_else(|| format!("missing slide part {}", slide_ref.partname))?;
        let relationships = part
            .rels
            .iter()
            .map(|relationship| {
                json!({
                    "id": relationship.r_id.to_string(),
                    "type": relationship.rel_type,
                    "target": relationship.target_ref,
                    "external": relationship.is_external,
                })
            })
            .collect::<Vec<_>>();
        let xml_text = String::from_utf8_lossy(xml);

        slides.push(json!({
            "index": index,
            "slide_id": slide_id,
            "selector": {"slide_id": slide_id},
            "part_name": slide_ref.partname.to_string(),
            "relationship_id": slide_ref.r_id.to_string(),
            "name": presentation.slide_name(slide_ref).map_err(|error| error.to_string())?,
            "layout": presentation.slide_layout_for(slide_ref).map_err(|error| error.to_string())?.map(|layout| json!({
                "name": layout.name,
                "part_name": layout.partname.to_string(),
                "master_part_name": layout.slide_master_part_name,
            })),
            "shapes": shapes,
            "transition": inspect_xml_element(&xml_text, "p:transition"),
            "animations": {
                "present": xml_text.contains("<p:timing"),
                "target_shape_ids": animation_targets(xml),
                "raw_timeline_preserved_until_modified": true,
            },
            "relationships": relationships,
        }));
    }

    let mut part_types: HashMap<String, usize> = HashMap::new();
    for part in presentation.package().parts() {
        *part_types
            .entry(format!(
                "{:?}",
                part_type_from_content_type(&part.content_type)
            ))
            .or_default() += 1;
    }

    Ok(json!({
        "engine": {
            "name": "aitui-native-rust",
            "model": "pptx",
            "version": "0.1",
            "external_runtime_required": false,
        },
        "presentation": {
            "slide_count": slides.len(),
            "slide_size_emu": slide_size.map(|(width, height)| json!({"width": width, "height": height})),
            "part_count": presentation.package().parts().count(),
            "part_types": part_types,
        },
        "slides": slides,
        "capabilities": {
            "read_only_inspect": true,
            "native_slide_selectors": true,
            "native_shape_selectors": true,
            "high_level_create_edit": "migration_in_progress",
            "exact_opc_edits": "aitui_guarded_opc_layer",
            "imported_animation_parse": false,
        },
        "preservation": {
            "source_mutated": false,
            "inspection_writes_package": false,
            "unknown_part_payloads_loaded_as_raw_bytes": true,
            "warnings": [
                "The pptx crate rewrites ZIP metadata, content-types XML, and relationship XML when saving.",
                "Typed shape reserialization is not assumed lossless for unsupported imported shape XML.",
                "Imported animation timelines remain raw slide XML unless an animation operation explicitly replaces them."
            ]
        }
    }))
}

/// Open an existing deck with the Rust engine and save it atomically.
///
/// Before replacing the destination, the serialized package is reopened and a
/// logical preservation manifest is compared with the source. This catches
/// dropped or rewritten part payloads and part-level relationships while still
/// allowing ZIP container metadata and entry order to change.
pub fn open_save(input: &Path, output: &Path) -> Result<Value, String> {
    let presentation = Presentation::open(input).map_err(|error| error.to_string())?;
    let source_manifest = package_manifest(&presentation);
    let bytes = presentation.to_bytes().map_err(|error| error.to_string())?;
    let reopened = Presentation::from_bytes(&bytes).map_err(|error| {
        format!("serialized package could not be reopened before commit: {error}")
    })?;
    let saved_manifest = package_manifest(&reopened);
    if source_manifest != saved_manifest {
        return Err(
            "native open-save preservation check failed; destination was not changed".into(),
        );
    }

    let temporary = temporary_path(output);
    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", temporary.display()))?;
        fs::rename(&temporary, output).map_err(|error| {
            format!(
                "cannot atomically replace {} with validated package: {error}",
                output.display()
            )
        })?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result?;

    Ok(json!({
        "engine": "aitui-native-rust",
        "operation": "open_save",
        "path": output,
        "slides": reopened.slide_count().map_err(|error| error.to_string())?,
        "preservation": {
            "part_payloads_equal": true,
            "part_relationships_equal": true,
            "reopened_before_commit": true,
            "atomic_destination_replace": true,
            "zip_metadata_may_differ": true,
        }
    }))
}

type RelationshipManifest = (String, String, String, bool);
type PartManifest = (String, Vec<u8>, Vec<RelationshipManifest>);

fn package_manifest(presentation: &Presentation) -> BTreeMap<String, PartManifest> {
    presentation
        .package()
        .parts()
        .map(|part| {
            let mut relationships = part
                .rels
                .iter()
                .map(|relationship| {
                    (
                        relationship.r_id.to_string(),
                        relationship.rel_type.to_string(),
                        relationship.target_ref.clone(),
                        relationship.is_external,
                    )
                })
                .collect::<Vec<_>>();
            relationships.sort();
            (
                part.partname.to_string(),
                (part.content_type.clone(), part.blob.clone(), relationships),
            )
        })
        .collect()
}

fn temporary_path(output: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("presentation.pptx");
    output.with_file_name(format!(".{name}.aitui-{}-{nonce}.tmp", std::process::id()))
}

fn inspect_shape(slide_id: u32, shape: &Shape) -> Value {
    let shape_id = shape.shape_id().0;
    let kind = match shape {
        Shape::AutoShape(_) => "autoshape",
        Shape::Picture(_) => "picture",
        Shape::GraphicFrame(frame) if frame.has_table => "table",
        Shape::GraphicFrame(frame) if frame.has_chart => "chart",
        Shape::GraphicFrame(_) => "graphic_frame",
        Shape::GroupShape(_) => "group",
        Shape::Connector(_) => "connector",
        Shape::OleObject(_) => "ole_object",
        _ => "unknown",
    };
    let text = shape
        .as_autoshape()
        .and_then(|shape| shape.text_frame())
        .map(|frame| frame.text());

    json!({
        "shape_id": shape_id,
        "selector": {"slide_id": slide_id, "shape_id": shape_id},
        "name": shape.name(),
        "kind": kind,
        "text": text,
        "placeholder": shape.is_placeholder(),
        "geometry": {
            "x_emu": shape.left().0,
            "y_emu": shape.top().0,
            "width_emu": shape.width().0,
            "height_emu": shape.height().0,
            "rotation_degrees": shape.rotation(),
        }
    })
}

fn parse_slide_ids(presentation: &Presentation) -> Result<HashMap<String, u32>, String> {
    let part = presentation
        .package()
        .parts()
        .find(|part| {
            part.content_type == PRESENTATION_CONTENT_TYPE
                || part_type_from_content_type(&part.content_type) == PartType::Presentation
        })
        .ok_or("presentation part not found")?;
    let mut reader = Reader::from_reader(part.blob.as_slice());
    reader.config_mut().trim_text(true);
    let mut ids = HashMap::new();
    let mut buffer = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Empty(element) | Event::Start(element))
                if local_name(element.name().as_ref()) == b"sldId" =>
            {
                let mut slide_id = None;
                let mut relationship_id = None;
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|error| error.to_string())?;
                    match local_name(attribute.key.as_ref()) {
                        b"id" if attribute.key.as_ref().contains(&b':') => {
                            relationship_id = Some(
                                String::from_utf8(attribute.value.into_owned())
                                    .map_err(|error| error.to_string())?,
                            );
                        }
                        b"id" => {
                            slide_id = Some(
                                std::str::from_utf8(attribute.value.as_ref())
                                    .map_err(|error| error.to_string())?
                                    .parse::<u32>()
                                    .map_err(|error| error.to_string())?,
                            );
                        }
                        _ => {}
                    }
                }
                if let (Some(relationship_id), Some(slide_id)) = (relationship_id, slide_id) {
                    ids.insert(relationship_id, slide_id);
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(error.to_string()),
            _ => {}
        }
        buffer.clear();
    }
    Ok(ids)
}

fn animation_targets(xml: &[u8]) -> Vec<u32> {
    let mut reader = Reader::from_reader(xml);
    let mut targets = Vec::new();
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Empty(element) | Event::Start(element))
                if local_name(element.name().as_ref()) == b"spTgt" =>
            {
                for attribute in element.attributes().flatten() {
                    if local_name(attribute.key.as_ref()) == b"spid" {
                        if let Ok(value) = std::str::from_utf8(attribute.value.as_ref()) {
                            if let Ok(value) = value.parse::<u32>() {
                                if !targets.contains(&value) {
                                    targets.push(value);
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buffer.clear();
    }
    targets
}

fn inspect_xml_element(xml: &str, element: &str) -> Value {
    let start = format!("<{element}");
    let Some(offset) = xml.find(&start) else {
        return Value::Null;
    };
    let tail = &xml[offset..];
    let end = tail.find('>').map_or(tail.len(), |index| index + 1);
    json!({"present": true, "opening_xml": &tail[..end]})
}

fn local_name(name: &[u8]) -> &[u8] {
    name.iter()
        .position(|byte| *byte == b':')
        .map_or(name, |index| &name[index + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "aitui-native-powerpoint-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn design_validation_rejects_accidental_overlap_and_allows_card_content() {
        let overlapping = serde_json::json!({
            "slides": [{
                "elements": [
                    {"id": "left", "type": "text", "x": 1.0, "y": 1.0, "width": 4.0, "height": 1.0, "text": "Left"},
                    {"id": "right", "type": "text", "x": 4.5, "y": 1.0, "width": 4.0, "height": 1.0, "text": "Right"}
                ]
            }]
        });
        let error = validate_deck_design(
            overlapping.as_object().unwrap(),
            overlapping["slides"].as_array().unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("overlap"), "{error}");

        let contained = serde_json::json!({
            "slides": [{
                "elements": [
                    {"id": "card", "type": "shape", "x": 1.0, "y": 1.0, "width": 5.0, "height": 3.0},
                    {"id": "label", "type": "text", "x": 1.4, "y": 1.4, "width": 4.0, "height": 0.8, "text": "Contained"}
                ]
            }]
        });
        assert!(validate_deck_design(
            contained.as_object().unwrap(),
            contained["slides"].as_array().unwrap(),
        )
        .is_ok());
    }

    #[test]
    fn continuity_groups_report_anchor_drift() {
        let spec = serde_json::json!({
            "slides": [
                {"continuity_group": "wire", "elements": [{"id": "wire", "type": "shape", "x": 1.0, "y": 3.0, "width": 9.0, "height": 0.5}]},
                {"continuity_group": "wire", "elements": [{"id": "wire", "type": "shape", "x": 1.5, "y": 3.0, "width": 9.0, "height": 0.5}]}
            ]
        });
        let diagnostics = validate_deck_design(
            spec.as_object().unwrap(),
            spec["slides"].as_array().unwrap(),
        )
        .unwrap();
        assert_eq!(diagnostics["warnings"][0]["code"], "CONTINUITY");
    }

    #[test]
    fn text_boxes_use_libreoffice_safe_transparent_multiline_xml() {
        let element = serde_json::json!({
            "id": "copy", "type": "text", "x": 1, "y": 1,
            "width": 5, "height": 2, "text": "First\nSecond"
        });
        let xml = shape_xml(
            element.as_object().unwrap(),
            2,
            "copy",
            (914_400, 914_400, 4_572_000, 1_828_800),
            "First\nSecond",
            true,
        );
        assert!(xml.contains("<a:noFill/>"));
        assert!(xml.contains("typeface=\"Liberation Sans\""));
        assert_eq!(xml.matches("<a:p>").count(), 2);
        assert!(!xml.contains("val=\"4472C4\""));
    }

    #[test]
    fn animations_default_to_one_click_but_explicit_mode_preserves_clicks() {
        let ids = HashMap::from([("a".to_string(), 2), ("b".to_string(), 3)]);
        let single = serde_json::json!({
            "animations": [
                {"type": "fade_in", "target": "a", "order": 0, "trigger": "on_click"},
                {"type": "fade_in", "target": "b", "order": 1, "trigger": "on_click"}
            ]
        });
        let single_xml = slide_effects_xml(single.as_object().unwrap(), &ids, 1).unwrap();
        assert_eq!(single_xml.matches("nodeType=\"clickEffect\"").count(), 1);
        assert_eq!(single_xml.matches("nodeType=\"afterEffect\"").count(), 1);

        let explicit = serde_json::json!({
            "animation_mode": "explicit",
            "animations": [
                {"type": "fade_in", "target": "a", "order": 0, "trigger": "on_click"},
                {"type": "fade_in", "target": "b", "order": 1, "trigger": "on_click"}
            ]
        });
        let explicit_xml = slide_effects_xml(explicit.as_object().unwrap(), &ids, 1).unwrap();
        assert_eq!(explicit_xml.matches("nodeType=\"clickEffect\"").count(), 2);
    }

    #[test]
    fn open_save_preserves_loaded_package_payloads_and_relationships() {
        let directory = temporary_directory("open-save");
        let source = directory.join("source.pptx");
        let output = directory.join("output.pptx");
        let presentation = Presentation::new().unwrap();
        presentation.save(&source).unwrap();
        let before = package_manifest(&Presentation::open(&source).unwrap());

        let result = open_save(&source, &output).unwrap();

        assert_eq!(result["operation"], "open_save");
        assert_eq!(result["preservation"]["part_payloads_equal"], true);
        assert_eq!(
            package_manifest(&Presentation::open(&output).unwrap()),
            before
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn open_save_does_not_replace_destination_when_source_is_invalid() {
        let directory = temporary_directory("atomic-refusal");
        let source = directory.join("invalid.pptx");
        let output = directory.join("existing.pptx");
        fs::write(&source, b"not a PowerPoint package").unwrap();
        fs::write(&output, b"existing destination").unwrap();

        assert!(open_save(&source, &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), b"existing destination");
        fs::remove_dir_all(directory).unwrap();
    }
}
