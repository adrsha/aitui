"""Advanced, atomic OPC/OOXML package modifiers for PowerPoint files.

These operations are the escape hatch beneath the friendly slide/element API. They
can address any XML part or package member, including features python-pptx does not
model, while retaining full-package XML/relationship validation and atomic
replacement of the destination.
"""

from __future__ import annotations

import base64
import os
from pathlib import Path, PurePosixPath
from tempfile import NamedTemporaryFile
from typing import Any, Sequence
from zipfile import ZIP_DEFLATED, ZipFile

from lxml import etree

from .validator import CONTENT_TYPES_NAMESPACE, RELATIONSHIPS_NAMESPACE, validate_package

PACKAGE_OPERATIONS = frozenset({
    "patch_xml", "put_part", "delete_part",
    "put_relationship", "delete_relationship",
    "set_content_type", "delete_content_type",
})
_XML_PARSER = etree.XMLParser(resolve_entities=False, no_network=True)


def _part_name(value: Any, *, allow_root: bool = False) -> str:
    if allow_root and value in (None, "", "/"):
        return ""
    if not isinstance(value, str) or not value.strip():
        raise ValueError("part must be a non-empty package-relative path")
    if "\\" in value or "\x00" in value:
        raise ValueError("part must use safe forward-slash package syntax")
    raw = value.lstrip("/")
    pieces = PurePosixPath(raw).parts
    if not pieces or any(piece in ("", ".", "..") for piece in pieces):
        raise ValueError("part must stay inside the PowerPoint package")
    return "/".join(pieces)


def _parse_xml(data: bytes, context: str) -> etree._Element:
    try:
        return etree.fromstring(data, parser=_XML_PARSER)
    except etree.XMLSyntaxError as error:
        raise ValueError(f"{context} is malformed: {error}") from error


def _xml_bytes(value: Any, field: str = "xml") -> bytes:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{field} must be a non-empty XML string")
    root = _parse_xml(value.encode("utf-8"), field)
    return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def _patch_xml(data: bytes, modifier: dict[str, Any]) -> bytes:
    root = _parse_xml(data, f"part {modifier.get('part')!r}")
    xpath = modifier.get("xpath")
    if not isinstance(xpath, str) or not xpath.strip():
        raise ValueError("patch_xml.xpath must be a non-empty XPath")
    namespaces = modifier.get("namespaces", {})
    if not isinstance(namespaces, dict) or not all(
        isinstance(key, str) and isinstance(value, str)
        for key, value in namespaces.items()
    ):
        raise TypeError("patch_xml.namespaces must map prefixes to namespace URIs")
    try:
        matches = root.xpath(xpath, namespaces=namespaces)
    except etree.XPathError as error:
        raise ValueError(f"invalid patch_xml XPath: {error}") from error
    if not matches:
        raise ValueError(f"patch_xml XPath matched no nodes: {xpath}")
    allow_multiple = modifier.get("allow_multiple", False)
    if not isinstance(allow_multiple, bool):
        raise TypeError("patch_xml.allow_multiple must be a boolean")
    if len(matches) != 1 and not allow_multiple:
        raise ValueError(
            f"patch_xml XPath matched {len(matches)} nodes; set allow_multiple=true explicitly"
        )
    action = modifier.get("action")
    for node in list(matches):
        if not isinstance(node, etree._Element):
            raise ValueError("patch_xml XPath must select XML elements")
        if action == "set_attributes":
            attributes = modifier.get("attributes")
            if not isinstance(attributes, dict) or not all(
                isinstance(key, str) and isinstance(value, (str, int, float, bool))
                for key, value in attributes.items()
            ):
                raise TypeError("set_attributes.attributes must be a scalar object")
            for key, value in attributes.items():
                node.set(key, str(value).lower() if isinstance(value, bool) else str(value))
        elif action == "remove_attributes":
            attributes = modifier.get("attributes")
            if not isinstance(attributes, list) or not all(isinstance(key, str) for key in attributes):
                raise TypeError("remove_attributes.attributes must be a string array")
            for key in attributes:
                node.attrib.pop(key, None)
        elif action == "set_text":
            text = modifier.get("text")
            if not isinstance(text, str):
                raise TypeError("set_text.text must be a string")
            node.text = text
        elif action in ("append_xml", "prepend_xml"):
            child = _parse_xml(_xml_bytes(modifier.get("xml")), "xml")
            if action == "append_xml":
                node.append(child)
            else:
                node.insert(0, child)
        elif action == "replace_xml":
            replacement = _parse_xml(_xml_bytes(modifier.get("xml")), "xml")
            parent = node.getparent()
            if parent is None:
                root = replacement
            else:
                parent.replace(node, replacement)
        elif action == "remove":
            parent = node.getparent()
            if parent is None:
                raise ValueError("cannot remove the document root; use put_part instead")
            parent.remove(node)
        else:
            raise ValueError(
                "patch_xml.action must be set_attributes, remove_attributes, set_text, "
                "append_xml, prepend_xml, replace_xml, or remove"
            )
    return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def _put_part(modifier: dict[str, Any]) -> bytes:
    encodings = [key for key in ("text", "xml", "base64") if key in modifier]
    if len(encodings) != 1:
        raise ValueError("put_part requires exactly one of text, xml, or base64")
    if encodings[0] == "text":
        value = modifier["text"]
        if not isinstance(value, str):
            raise TypeError("put_part.text must be a string")
        return value.encode("utf-8")
    if encodings[0] == "xml":
        return _xml_bytes(modifier["xml"])
    value = modifier["base64"]
    if not isinstance(value, str):
        raise TypeError("put_part.base64 must be a string")
    try:
        return base64.b64decode(value, validate=True)
    except ValueError as error:
        raise ValueError("put_part.base64 is not valid base64") from error


def _relationships_part(source_part: str) -> str:
    if not source_part:
        return "_rels/.rels"
    source = PurePosixPath(source_part)
    return str(source.parent / "_rels" / f"{source.name}.rels")


def _relationship_root(members: dict[str, bytes], rels_part: str) -> etree._Element:
    if rels_part in members:
        root = _parse_xml(members[rels_part], f"relationship part {rels_part}")
        if root.tag != f"{{{RELATIONSHIPS_NAMESPACE}}}Relationships":
            raise ValueError(f"relationship part has an invalid root element: {rels_part}")
        return root
    return etree.Element(f"{{{RELATIONSHIPS_NAMESPACE}}}Relationships", nsmap={None: RELATIONSHIPS_NAMESPACE})


def _put_relationship(members: dict[str, bytes], modifier: dict[str, Any]) -> None:
    source = _part_name(modifier.get("source_part"), allow_root=True)
    if source and source not in members:
        raise FileNotFoundError(f"relationship source part does not exist: {source}")
    relationship_id = modifier.get("id")
    relationship_type = modifier.get("relationship_type")
    target = modifier.get("target")
    if not isinstance(relationship_id, str) or not relationship_id.strip():
        raise ValueError("put_relationship.id must be a non-empty string")
    if not isinstance(relationship_type, str) or not relationship_type.strip():
        raise ValueError("put_relationship.relationship_type must be a non-empty string")
    if not isinstance(target, str) or not target.strip() or "\\" in target or "\x00" in target:
        raise ValueError("put_relationship.target must be a safe non-empty URI")
    target_mode = modifier.get("target_mode")
    if target_mode not in (None, "Internal", "External"):
        raise ValueError("put_relationship.target_mode must be Internal or External")
    rels_part = _relationships_part(source)
    root = _relationship_root(members, rels_part)
    matches = root.xpath("./pr:Relationship[@Id=$id]", namespaces={"pr": RELATIONSHIPS_NAMESPACE}, id=relationship_id)
    if matches and not modifier.get("replace", False):
        raise ValueError(f"relationship ID already exists in {rels_part}: {relationship_id}")
    if modifier.get("replace", False) not in (True, False):
        raise TypeError("put_relationship.replace must be a boolean")
    relationship = matches[0] if matches else etree.SubElement(root, f"{{{RELATIONSHIPS_NAMESPACE}}}Relationship")
    relationship.attrib.clear()
    relationship.set("Id", relationship_id)
    relationship.set("Type", relationship_type)
    relationship.set("Target", target)
    if target_mode == "External":
        relationship.set("TargetMode", "External")
    members[rels_part] = etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def _delete_relationship(members: dict[str, bytes], modifier: dict[str, Any]) -> None:
    source = _part_name(modifier.get("source_part"), allow_root=True)
    relationship_id = modifier.get("id")
    if not isinstance(relationship_id, str) or not relationship_id.strip():
        raise ValueError("delete_relationship.id must be a non-empty string")
    rels_part = _relationships_part(source)
    if rels_part not in members:
        raise FileNotFoundError(f"relationship part does not exist: {rels_part}")
    root = _relationship_root(members, rels_part)
    matches = root.xpath("./pr:Relationship[@Id=$id]", namespaces={"pr": RELATIONSHIPS_NAMESPACE}, id=relationship_id)
    if len(matches) != 1:
        raise ValueError(f"relationship ID does not exist in {rels_part}: {relationship_id}")
    root.remove(matches[0])
    members[rels_part] = etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def _content_types_root(members: dict[str, bytes]) -> etree._Element:
    try:
        root = _parse_xml(members["[Content_Types].xml"], "[Content_Types].xml")
    except KeyError as error:
        raise ValueError("package is missing [Content_Types].xml") from error
    if root.tag != f"{{{CONTENT_TYPES_NAMESPACE}}}Types":
        raise ValueError("[Content_Types].xml has an invalid root element")
    return root


def _set_content_type(members: dict[str, bytes], modifier: dict[str, Any]) -> None:
    content_type = modifier.get("content_type")
    if not isinstance(content_type, str) or not content_type.strip():
        raise ValueError("set_content_type.content_type must be a non-empty string")
    root = _content_types_root(members)
    part = modifier.get("part")
    extension = modifier.get("extension")
    if (part is None) == (extension is None):
        raise ValueError("set_content_type requires exactly one of part or extension")
    if part is not None:
        normalized = "/" + _part_name(part)
        tag, key, value = "Override", "PartName", normalized
    else:
        if not isinstance(extension, str) or not extension.strip() or "/" in extension or "." in extension:
            raise ValueError("set_content_type.extension must be an extension without a dot")
        tag, key, value = "Default", "Extension", extension.lower()
    matches = root.xpath(f"./ct:{tag}[@{key}=$value]", namespaces={"ct": CONTENT_TYPES_NAMESPACE}, value=value)
    entry = matches[0] if matches else etree.SubElement(root, f"{{{CONTENT_TYPES_NAMESPACE}}}{tag}")
    entry.set(key, value)
    entry.set("ContentType", content_type)
    members["[Content_Types].xml"] = etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def _delete_content_type(members: dict[str, bytes], modifier: dict[str, Any]) -> None:
    root = _content_types_root(members)
    part = modifier.get("part")
    extension = modifier.get("extension")
    if (part is None) == (extension is None):
        raise ValueError("delete_content_type requires exactly one of part or extension")
    if part is not None:
        tag, key, value = "Override", "PartName", "/" + _part_name(part)
    else:
        if not isinstance(extension, str) or not extension.strip():
            raise ValueError("delete_content_type.extension must be a non-empty string")
        tag, key, value = "Default", "Extension", extension.lower().lstrip(".")
    matches = root.xpath(f"./ct:{tag}[@{key}=$value]", namespaces={"ct": CONTENT_TYPES_NAMESPACE}, value=value)
    if len(matches) != 1:
        raise ValueError(f"content type declaration does not exist for {value}")
    root.remove(matches[0])
    members["[Content_Types].xml"] = etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def apply_package_modifiers(
    input_path: str | Path,
    output_path: str | Path,
    modifiers: Sequence[dict[str, Any]],
    *,
    expected_slide_count: int | None = None,
) -> Path:
    """Apply ordered package modifiers atomically and validate the result."""
    source, destination = Path(input_path), Path(output_path)
    with ZipFile(source) as archive:
        names = [info.filename for info in archive.infolist()]
        if len(names) != len(set(names)):
            raise ValueError("input package contains duplicate ZIP member names")
        members = {name: archive.read(name) for name in names}
    for modifier in modifiers:
        if not isinstance(modifier, dict):
            raise TypeError("package modifiers must be objects")
        operation = modifier.get("operation")
        if operation in ("patch_xml", "put_part", "delete_part"):
            part = _part_name(modifier.get("part"))
            if part == "[Content_Types].xml" and operation == "delete_part":
                raise ValueError("cannot delete [Content_Types].xml")
            if operation == "patch_xml":
                if part not in members:
                    raise FileNotFoundError(f"package part does not exist: {part}")
                members[part] = _patch_xml(members[part], modifier)
            elif operation == "put_part":
                members[part] = _put_part(modifier)
            else:
                if part not in members:
                    raise FileNotFoundError(f"package part does not exist: {part}")
                del members[part]
        elif operation == "put_relationship":
            _put_relationship(members, modifier)
        elif operation == "delete_relationship":
            _delete_relationship(members, modifier)
        elif operation == "set_content_type":
            _set_content_type(members, modifier)
        elif operation == "delete_content_type":
            _delete_content_type(members, modifier)
        else:
            raise ValueError(f"unsupported package operation: {operation!r}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary_path: Path | None = None
    try:
        with NamedTemporaryFile(
            prefix=f".{destination.stem}-", suffix=".pptx",
            dir=destination.parent, delete=False,
        ) as temporary:
            temporary_path = Path(temporary.name)
        with ZipFile(temporary_path, "w", ZIP_DEFLATED) as archive:
            for name, data in members.items():
                archive.writestr(name, data)
        validate_package(temporary_path, expected_slide_count=expected_slide_count)
        os.replace(temporary_path, destination)
        temporary_path = None
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)
    return destination
