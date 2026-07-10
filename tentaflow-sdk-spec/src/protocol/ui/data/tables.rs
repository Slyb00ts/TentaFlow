// =============================================================================
// File: protocol/ui/data/tables.rs — Table/List/Tree/EmptyCell (catalog §4)
// =============================================================================

use super::super::super::value::Value;
use super::super::actions::Button;
use super::super::bind::{BindRef, StatePath};
use super::super::component::{Component, FieldMap};
use super::super::inline::{TableColumn, TablePagination};
use super::super::molecules::EmptyState;
use super::super::tokens::{Density, EmptyCellVariant, TableSelectMode, TableVariant, TreeVariant};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_ref_tag_decode,
    ensure_ref_tag_encode, ensure_tag, missing_field, unknown_field, IntoComponentError,
};

#[inline]
fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component {
        tag,
        id: id.into(),
        fields: FieldMap(fields),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    }
}

// -----------------------------------------------------------------------------
// 0x0211 — Table
// -----------------------------------------------------------------------------

/// Powerful data table (catalog §4 0x0211). Handlers: `"row_click"`,
/// `"row_double_click"`, `"selection_change"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: Vec<TableColumn>,
    pub rows_path: StatePath,
    pub row_key_field: String,
    pub variant: TableVariant,
    pub density: Density,
    pub sortable: bool,
    pub sort_by: Option<BindRef>,
    pub selectable: TableSelectMode,
    pub selected_ids: Option<BindRef>,
    pub sticky_header: bool,
    pub sticky_columns: u8,
    pub pagination: Option<TablePagination>,
    /// `ComponentRef<EmptyState>` (tag 0x0003).
    pub empty_state: Option<Component>,
    /// `ComponentRef<Button>` entries — per-row action menu.
    pub row_actions: Vec<Component>,
    /// `ComponentRef<Button>` entries — shown when rows selected.
    pub bulk_actions: Vec<Component>,
    pub virtualize: bool,
    pub row_expandable: bool,
    pub expanded_row_template_id: Option<String>,
}

impl Table {
    pub const TAG: u16 = 0x0211;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        if let Some(es) = &self.empty_state {
            ensure_ref_tag_encode(es.tag, EmptyState::TAG, "Table", "empty_state")?;
        }
        for b in &self.row_actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "Table", "row_actions")?;
        }
        for b in &self.bulk_actions {
            ensure_ref_tag_encode(b.tag, Button::TAG, "Table", "bulk_actions")?;
        }
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(18);
        e.push((0, encode_to_value(&self.columns)?));
        e.push((1, encode_to_value(&self.rows_path)?));
        e.push((2, encode_to_value(&self.row_key_field)?));
        e.push((3, encode_to_value(&self.variant)?));
        e.push((4, encode_to_value(&self.density)?));
        e.push((5, encode_to_value(&self.sortable)?));
        if let Some(s) = &self.sort_by {
            e.push((6, encode_to_value(s)?));
        }
        e.push((7, encode_to_value(&self.selectable)?));
        if let Some(s) = &self.selected_ids {
            e.push((8, encode_to_value(s)?));
        }
        e.push((9, encode_to_value(&self.sticky_header)?));
        e.push((10, encode_to_value(&self.sticky_columns)?));
        if let Some(p) = &self.pagination {
            e.push((11, encode_to_value(p)?));
        }
        if let Some(es) = &self.empty_state {
            e.push((12, encode_to_value(es)?));
        }
        e.push((13, encode_to_value(&self.row_actions)?));
        e.push((14, encode_to_value(&self.bulk_actions)?));
        e.push((15, encode_to_value(&self.virtualize)?));
        e.push((16, encode_to_value(&self.row_expandable)?));
        if let Some(t) = &self.expanded_row_template_id {
            e.push((17, encode_to_value(t)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Table")?;
        ensure_no_duplicate_keys("Table", &c.fields.0)?;
        let mut columns = None;
        let mut rows_path = None;
        let mut row_key_field = None;
        let mut variant = None;
        let mut density = None;
        let mut sortable = None;
        let mut sort_by = None;
        let mut selectable = None;
        let mut selected_ids = None;
        let mut sticky_header = None;
        let mut sticky_columns = None;
        let mut pagination = None;
        let mut empty_state = None;
        let mut row_actions = None;
        let mut bulk_actions = None;
        let mut virtualize = None;
        let mut row_expandable = None;
        let mut expanded_row_template_id = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => columns = Some(decode_from_value(v)?),
                1 => rows_path = Some(decode_from_value(v)?),
                2 => row_key_field = Some(decode_from_value(v)?),
                3 => variant = Some(decode_from_value(v)?),
                4 => density = Some(decode_from_value(v)?),
                5 => sortable = Some(decode_from_value(v)?),
                6 => sort_by = Some(decode_from_value(v)?),
                7 => selectable = Some(decode_from_value(v)?),
                8 => selected_ids = Some(decode_from_value(v)?),
                9 => sticky_header = Some(decode_from_value(v)?),
                10 => sticky_columns = Some(decode_from_value(v)?),
                11 => pagination = Some(decode_from_value(v)?),
                12 => empty_state = Some(decode_from_value(v)?),
                13 => row_actions = Some(decode_from_value(v)?),
                14 => bulk_actions = Some(decode_from_value(v)?),
                15 => virtualize = Some(decode_from_value(v)?),
                16 => row_expandable = Some(decode_from_value(v)?),
                17 => expanded_row_template_id = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Table", *other)),
            }
        }
        Ok(Table {
            columns: columns.unwrap_or_default(),
            rows_path: rows_path.ok_or_else(|| missing_field("Table", "rows_path"))?,
            row_key_field: row_key_field.ok_or_else(|| missing_field("Table", "row_key_field"))?,
            variant: variant.ok_or_else(|| missing_field("Table", "variant"))?,
            density: density.ok_or_else(|| missing_field("Table", "density"))?,
            sortable: sortable.ok_or_else(|| missing_field("Table", "sortable"))?,
            sort_by,
            selectable: selectable.ok_or_else(|| missing_field("Table", "selectable"))?,
            selected_ids,
            sticky_header: sticky_header.ok_or_else(|| missing_field("Table", "sticky_header"))?,
            sticky_columns: sticky_columns
                .ok_or_else(|| missing_field("Table", "sticky_columns"))?,
            pagination,
            empty_state: {
                let es: Option<Component> = empty_state;
                if let Some(c) = &es {
                    ensure_ref_tag_decode(c.tag, EmptyState::TAG, "Table", "empty_state")?;
                }
                es
            },
            row_actions: {
                let v: Vec<Component> = row_actions.unwrap_or_default();
                for b in &v {
                    ensure_ref_tag_decode(b.tag, Button::TAG, "Table", "row_actions")?;
                }
                v
            },
            bulk_actions: {
                let v: Vec<Component> = bulk_actions.unwrap_or_default();
                for b in &v {
                    ensure_ref_tag_decode(b.tag, Button::TAG, "Table", "bulk_actions")?;
                }
                v
            },
            virtualize: virtualize.ok_or_else(|| missing_field("Table", "virtualize"))?,
            row_expandable: row_expandable
                .ok_or_else(|| missing_field("Table", "row_expandable"))?,
            expanded_row_template_id,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0212 — List
// -----------------------------------------------------------------------------

/// Lightweight virtual/non-virtual list (catalog §4 0x0212). Handler: `"item_click"`.
#[derive(Debug, Clone, PartialEq)]
pub struct List {
    pub items_path: StatePath,
    pub item_template_id: String,
    pub divider: bool,
    pub density: Density,
    pub virtualize: bool,
    /// `ComponentRef<EmptyState>` (tag 0x0003).
    pub empty_state: Option<Component>,
    pub max_visible: Option<u32>,
}

impl List {
    pub const TAG: u16 = 0x0212;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        if let Some(es) = &self.empty_state {
            ensure_ref_tag_encode(es.tag, EmptyState::TAG, "List", "empty_state")?;
        }
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(7);
        e.push((0, encode_to_value(&self.items_path)?));
        e.push((1, encode_to_value(&self.item_template_id)?));
        e.push((2, encode_to_value(&self.divider)?));
        e.push((3, encode_to_value(&self.density)?));
        e.push((4, encode_to_value(&self.virtualize)?));
        if let Some(es) = &self.empty_state {
            e.push((5, encode_to_value(es)?));
        }
        if let Some(m) = &self.max_visible {
            e.push((6, encode_to_value(m)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "List")?;
        ensure_no_duplicate_keys("List", &c.fields.0)?;
        let mut items_path = None;
        let mut item_template_id = None;
        let mut divider = None;
        let mut density = None;
        let mut virtualize = None;
        let mut empty_state = None;
        let mut max_visible = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => items_path = Some(decode_from_value(v)?),
                1 => item_template_id = Some(decode_from_value(v)?),
                2 => divider = Some(decode_from_value(v)?),
                3 => density = Some(decode_from_value(v)?),
                4 => virtualize = Some(decode_from_value(v)?),
                5 => empty_state = Some(decode_from_value(v)?),
                6 => max_visible = Some(decode_from_value(v)?),
                other => return Err(unknown_field("List", *other)),
            }
        }
        Ok(List {
            items_path: items_path.ok_or_else(|| missing_field("List", "items_path"))?,
            item_template_id: item_template_id
                .ok_or_else(|| missing_field("List", "item_template_id"))?,
            divider: divider.ok_or_else(|| missing_field("List", "divider"))?,
            density: density.ok_or_else(|| missing_field("List", "density"))?,
            virtualize: virtualize.ok_or_else(|| missing_field("List", "virtualize"))?,
            empty_state: {
                let es: Option<Component> = empty_state;
                if let Some(c) = &es {
                    ensure_ref_tag_decode(c.tag, EmptyState::TAG, "List", "empty_state")?;
                }
                es
            },
            max_visible,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0213 — Tree
// -----------------------------------------------------------------------------

/// Hierarchical data tree (catalog §4 0x0213). Handlers: `"expand"`, `"collapse"`, `"select"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    pub nodes_path: StatePath,
    pub expanded_ids: BindRef,
    pub selected_id: Option<BindRef>,
    pub variant: TreeVariant,
    pub lazy_load: bool,
}

impl Tree {
    pub const TAG: u16 = 0x0213;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.nodes_path)?));
        e.push((1, encode_to_value(&self.expanded_ids)?));
        if let Some(s) = &self.selected_id {
            e.push((2, encode_to_value(s)?));
        }
        e.push((3, encode_to_value(&self.variant)?));
        e.push((4, encode_to_value(&self.lazy_load)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Tree")?;
        ensure_no_duplicate_keys("Tree", &c.fields.0)?;
        let mut nodes_path = None;
        let mut expanded_ids = None;
        let mut selected_id = None;
        let mut variant = None;
        let mut lazy_load = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => nodes_path = Some(decode_from_value(v)?),
                1 => expanded_ids = Some(decode_from_value(v)?),
                2 => selected_id = Some(decode_from_value(v)?),
                3 => variant = Some(decode_from_value(v)?),
                4 => lazy_load = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Tree", *other)),
            }
        }
        Ok(Tree {
            nodes_path: nodes_path.ok_or_else(|| missing_field("Tree", "nodes_path"))?,
            expanded_ids: expanded_ids.ok_or_else(|| missing_field("Tree", "expanded_ids"))?,
            selected_id,
            variant: variant.ok_or_else(|| missing_field("Tree", "variant"))?,
            lazy_load: lazy_load.ok_or_else(|| missing_field("Tree", "lazy_load"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x0214 — EmptyCell
// -----------------------------------------------------------------------------

/// Nullish-value placeholder for tables/lists (catalog §4 0x0214).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmptyCell {
    pub variant: EmptyCellVariant,
}

impl EmptyCell {
    pub const TAG: u16 = 0x0214;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(1);
        e.push((0, encode_to_value(&self.variant)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "EmptyCell")?;
        ensure_no_duplicate_keys("EmptyCell", &c.fields.0)?;
        let mut variant = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => variant = Some(decode_from_value(v)?),
                other => return Err(unknown_field("EmptyCell", *other)),
            }
        }
        Ok(EmptyCell {
            variant: variant.ok_or_else(|| missing_field("EmptyCell", "variant"))?,
        })
    }
}
