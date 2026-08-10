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
