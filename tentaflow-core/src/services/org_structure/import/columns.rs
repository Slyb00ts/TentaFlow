//! The columns of the import and export file, defined once.
//!
//! The export writes the Polish `header` of each column, or the English
//! `header_en` for a reader whose language is not Polish; the import accepts
//! both and every alias and ignores case, diacritics,
//! spaces and punctuation, because the file is edited in a spreadsheet.

use super::report::FileError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    UnitCode,
    UnitName,
    ParentCode,
    UnitType,
    PositionCode,
    Position,
    Staff,
    Head,
    Primary,
    Person,
    PersonName,
    Email,
    Share,
    Manager,
    DeputyOrder,
    From,
}

pub struct ColumnSpec {
    pub column: Column,
    /// The header the export writes for a Polish reader.
    pub header: &'static str,
    /// The header the export writes for everyone else; an alias too.
    pub header_en: &'static str,
    /// Stable identifier reports name the column by; screens translate it.
    pub key: &'static str,
    pub aliases: &'static [&'static str],
}

/// In the order of the export.
pub const COLUMNS: [ColumnSpec; 16] = [
    ColumnSpec {
        column: Column::UnitCode,
        header: "kod jednostki",
        header_en: "unit code",
        key: "unit_code",
        aliases: &["unit code", "kod działu"],
    },
    ColumnSpec {
        column: Column::UnitName,
        header: "nazwa jednostki",
        header_en: "unit name",
        key: "unit_name",
        aliases: &["unit name", "jednostka", "unit"],
    },
    ColumnSpec {
        column: Column::ParentCode,
        header: "kod nadrzędnej",
        header_en: "parent code",
        key: "parent_code",
        aliases: &[
            "kod jednostki nadrzędnej",
            "parent code",
            "parent unit code",
            "parent",
        ],
    },
    ColumnSpec {
        column: Column::UnitType,
        header: "typ",
        header_en: "unit type",
        key: "unit_type",
        aliases: &["typ jednostki", "type", "unit type"],
    },
    ColumnSpec {
        column: Column::PositionCode,
        header: "kod stanowiska",
        header_en: "position code",
        key: "position_code",
        aliases: &["position code"],
    },
    ColumnSpec {
        column: Column::Position,
        header: "stanowisko",
        header_en: "position",
        key: "position",
        aliases: &["nazwa stanowiska", "position", "position name", "title"],
    },
    ColumnSpec {
        column: Column::Staff,
        header: "sztabowe",
        header_en: "staff",
        key: "staff",
        aliases: &["stanowisko sztabowe", "staff", "is staff", "staff position"],
    },
    ColumnSpec {
        column: Column::Head,
        header: "kierownik jednostki",
        header_en: "unit head",
        key: "head",
        aliases: &["kierownik", "head", "unit head", "head of unit"],
    },
    ColumnSpec {
        column: Column::Primary,
        header: "stanowisko główne",
        header_en: "primary",
        key: "primary",
        aliases: &["główne", "primary", "is primary", "main position"],
    },
    ColumnSpec {
        column: Column::Person,
        header: "login/e-mail osoby",
        header_en: "login or email",
        key: "person",
        aliases: &[
            "login",
            "login/e-mail",
            "login lub e-mail",
            "login or email",
            "person",
            "użytkownik",
            "user",
        ],
    },
    ColumnSpec {
        column: Column::PersonName,
        header: "osoba",
        header_en: "person name",
        key: "person_name",
        aliases: &["imię i nazwisko", "person name", "full name", "name"],
    },
    ColumnSpec {
        column: Column::Email,
        header: "e-mail",
        header_en: "email",
        key: "email",
        aliases: &["adres e-mail", "mail"],
    },
    ColumnSpec {
        column: Column::Share,
        header: "część etatu",
        header_en: "share",
        key: "share",
        aliases: &["etat", "share", "fte", "time share"],
    },
    ColumnSpec {
        column: Column::Manager,
        header: "przełożony (kod stanowiska)",
        header_en: "manager code",
        key: "manager",
        aliases: &[
            "przełożony",
            "kod przełożonego",
            "manager",
            "manager code",
            "reports to",
        ],
    },
    ColumnSpec {
        column: Column::DeputyOrder,
        header: "zastępca kierownika (kolejność)",
        header_en: "deputy order",
        key: "deputy_order",
        aliases: &[
            "zastępca kierownika",
            "zastępca",
            "deputy",
            "deputy head",
            "deputy order",
        ],
    },
    ColumnSpec {
        column: Column::From,
        header: "od kiedy",
        header_en: "valid from",
        key: "from",
        aliases: &[
            "data od",
            "od",
            "from",
            "valid from",
            "effective from",
            "start date",
        ],
    },
];

/// The language of the header row an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderLanguage {
    Polish,
    English,
}

impl HeaderLanguage {
    /// The language of a user's stored preference. No preference is Polish:
    /// that is the language the dashboard shows until someone changes it.
    pub fn of_preference(preferred: Option<&str>) -> Self {
        match preferred {
            None => Self::Polish,
            Some(code) if code.trim().to_ascii_lowercase().starts_with("pl") => Self::Polish,
            Some(_) => Self::English,
        }
    }
}

impl Column {
    pub fn header(self, language: HeaderLanguage) -> &'static str {
        match language {
            HeaderLanguage::Polish => self.spec().header,
            HeaderLanguage::English => self.spec().header_en,
        }
    }

    pub fn spec(self) -> &'static ColumnSpec {
        COLUMNS
            .iter()
            .find(|spec| spec.column == self)
            .expect("every column is listed in COLUMNS")
    }

    pub fn key(self) -> &'static str {
        self.spec().key
    }
}

/// Lowercase, Polish letters without diacritics, letters and digits only:
/// "Kod nadrzędnej ", "kod_nadrzednej" and "KOD-NADRZĘDNEJ" are one header.
pub fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            other => other,
        })
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn column_named(header: &str) -> Option<Column> {
    let folded = fold(header);
    if folded.is_empty() {
        return None;
    }
    COLUMNS
        .iter()
        .find(|spec| {
            fold(spec.header) == folded || spec.aliases.iter().any(|alias| fold(alias) == folded)
        })
        .map(|spec| spec.column)
}

/// Where each known column sits in the file.
pub struct Mapping {
    index: [Option<usize>; COLUMNS.len()],
    /// Headers that name no column; reported, not fatal.
    pub ignored: Vec<String>,
}

impl Mapping {
    pub fn from_headers(headers: &[String]) -> Result<Self, FileError> {
        let mut index = [None; COLUMNS.len()];
        let mut ignored = Vec::new();
        for (position, header) in headers.iter().enumerate() {
            let Some(column) = column_named(header) else {
                if !header.trim().is_empty() {
                    ignored.push(header.trim().to_string());
                }
                continue;
            };
            let slot = &mut index[column as usize];
            if slot.is_some() {
                return Err(FileError::DuplicateColumn(column));
            }
            *slot = Some(position);
        }
        if index[Column::UnitCode as usize].is_none() {
            return Err(FileError::MissingColumn(Column::UnitCode));
        }
        Ok(Self { index, ignored })
    }

    pub fn cell<'a>(&self, column: Column, cells: &'a [String]) -> &'a str {
        self.index[column as usize]
            .and_then(|position| cells.get(position))
            .map_or("", String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn every_header_and_alias_names_exactly_one_column() {
        let mut seen: HashMap<String, Column> = HashMap::new();
        for spec in &COLUMNS {
            for name in std::iter::once(&spec.header).chain(spec.aliases) {
                let folded = fold(name);
                assert!(!folded.is_empty(), "{name} folds to nothing");
                if let Some(other) = seen.insert(folded, spec.column) {
                    assert_eq!(other, spec.column, "'{name}' names two columns");
                }
            }
        }
    }

    #[test]
    fn columns_are_indexed_by_declaration_order() {
        for (position, spec) in COLUMNS.iter().enumerate() {
            assert_eq!(spec.column as usize, position, "{}", spec.key);
        }
    }

    #[test]
    fn a_header_ignores_case_diacritics_spaces_and_punctuation() {
        for header in [
            "Kod nadrzędnej ",
            "kod_nadrzednej",
            "KOD-NADRZĘDNEJ",
            "Parent Code",
        ] {
            assert_eq!(column_named(header), Some(Column::ParentCode), "{header}");
        }
        assert_eq!(column_named("Login/E-mail osoby"), Some(Column::Person));
        assert_eq!(column_named("nothing like it"), None);
    }
}
