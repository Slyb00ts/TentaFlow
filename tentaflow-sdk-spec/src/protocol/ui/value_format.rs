// =============================================================================
// File: protocol/ui/value_format.rs — localised display format (catalog §1.3)
// Purpose: ValueFormat discriminated union + its style sub-enums. Renderer
// uses these to format BindRef values before rendering text.
// =============================================================================

use crate::protocol::ui::typed_field::assert_no_dup_tstr;
use minicbor::{Decode, Decoder, Encode, Encoder};

string_enum! {
    /// Base for byte formatting: SI (1000) or binary (1024).
    pub enum BytesBase {
        Si = "1000",
        Binary = "1024",
    }
}

string_enum! {
    /// Duration formatting style.
    pub enum DurationStyle {
        Short = "short",
        Long = "long",
        Stopwatch = "stopwatch",
    }
}

string_enum! {
    /// Date formatting style.
    pub enum DateStyle {
        Short = "short",
        Medium = "medium",
        Long = "long",
        Full = "full",
    }
}

string_enum! {
    /// Time formatting style.
    pub enum TimeStyle {
        Short = "short",
        Medium = "medium",
        Long = "long",
    }
}

string_enum! {
    /// Combined date+time formatting style.
    pub enum DateTimeStyle {
        Short = "short",
        Medium = "medium",
        Long = "long",
        Full = "full",
    }
}

/// Locale-aware display format applied by the renderer to bound values.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueFormat {
    Number { decimals: u8, thousands_sep: bool },
    Currency { code: String },
    Percent { decimals: u8 },
    Bytes { base: BytesBase },
    Duration { style: DurationStyle },
    Date { style: DateStyle },
    Time { style: TimeStyle },
    DateTime { style: DateTimeStyle },
    Relative,
    Plain,
}

impl<C> Encode<C> for ValueFormat {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key ordering when present (tstr keys):
        //   "base"(0x64..) < "code"(0x64..) < "kind"(0x64..) < "style"(0x65..)
        //   < "decimals"(0x68..) < "thousands_sep"(0x6d..)
        // Encoders below emit only the variant-relevant keys, sorted ascending.
        match self {
            ValueFormat::Number {
                decimals,
                thousands_sep,
            } => {
                // Canonical: kind (0x64..) < decimals (0x68..) < thousands_sep (0x6d..).
                e.map(3)?;
                e.str("kind")?.str("number")?;
                e.str("decimals")?.u8(*decimals)?;
                e.str("thousands_sep")?.bool(*thousands_sep)?;
            }
            ValueFormat::Currency { code } => {
                // Canonical: code (0x64..) < kind (0x64..). Compare bytes:
                //   "code" = 0x64 0x63 ...
                //   "kind" = 0x64 0x6b ...
                //   → "code" < "kind".
                e.map(2)?;
                e.str("code")?.str(code)?;
                e.str("kind")?.str("currency")?;
            }
            ValueFormat::Percent { decimals } => {
                // Canonical: kind (0x64..) < decimals (0x68..).
                e.map(2)?;
                e.str("kind")?.str("percent")?;
                e.str("decimals")?.u8(*decimals)?;
            }
            ValueFormat::Bytes { base } => {
                // Canonical: base (0x64..) < kind (0x64..). Compare bytes:
                //   "base" = 0x64 0x62 ...; "kind" = 0x64 0x6b ... → base < kind.
                e.map(2)?;
                e.str("base")?;
                base.encode(e, ctx)?;
                e.str("kind")?.str("bytes")?;
            }
            ValueFormat::Duration { style } => {
                e.map(2)?;
                e.str("kind")?.str("duration")?;
                e.str("style")?;
                style.encode(e, ctx)?;
            }
            ValueFormat::Date { style } => {
                e.map(2)?;
                e.str("kind")?.str("date")?;
                e.str("style")?;
                style.encode(e, ctx)?;
            }
            ValueFormat::Time { style } => {
                e.map(2)?;
                e.str("kind")?.str("time")?;
                e.str("style")?;
                style.encode(e, ctx)?;
            }
            ValueFormat::DateTime { style } => {
                e.map(2)?;
                e.str("kind")?.str("datetime")?;
                e.str("style")?;
                style.encode(e, ctx)?;
            }
            ValueFormat::Relative => {
                e.map(1)?;
                e.str("kind")?.str("relative")?;
            }
            ValueFormat::Plain => {
                e.map(1)?;
                e.str("kind")?.str("plain")?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for ValueFormat {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut decimals: Option<u8> = None;
        let mut thousands_sep: Option<bool> = None;
        let mut code: Option<String> = None;
        let mut base: Option<BytesBase> = None;
        let mut style_raw: Option<String> = None;
        for _ in 0..len {
            let key = d.str()?;
            match key {
                "kind" => {
                    assert_no_dup_tstr(&kind, "ValueFormat", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "decimals" => {
                    assert_no_dup_tstr(&decimals, "ValueFormat", "decimals")?;
                    decimals = Some(d.u8()?);
                }
                "thousands_sep" => {
                    assert_no_dup_tstr(&thousands_sep, "ValueFormat", "thousands_sep")?;
                    thousands_sep = Some(d.bool()?);
                }
                "code" => {
                    assert_no_dup_tstr(&code, "ValueFormat", "code")?;
                    code = Some(d.str()?.to_string());
                }
                "base" => {
                    assert_no_dup_tstr(&base, "ValueFormat", "base")?;
                    base = Some(BytesBase::decode(d, ctx)?);
                }
                "style" => {
                    assert_no_dup_tstr(&style_raw, "ValueFormat", "style")?;
                    style_raw = Some(d.str()?.to_string());
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown ValueFormat key: {other}"
                    )))
                }
            }
        }
        let resolve_duration_style = || -> Result<DurationStyle, minicbor::decode::Error> {
            let raw = style_raw.as_deref().ok_or_else(|| {
                minicbor::decode::Error::message("ValueFormat.duration missing style")
            })?;
            DurationStyle::from_wire(raw)
                .ok_or_else(|| minicbor::decode::Error::message("invalid DurationStyle"))
        };
        let resolve_date_style = || -> Result<DateStyle, minicbor::decode::Error> {
            let raw = style_raw.as_deref().ok_or_else(|| {
                minicbor::decode::Error::message("ValueFormat.date missing style")
            })?;
            DateStyle::from_wire(raw)
                .ok_or_else(|| minicbor::decode::Error::message("invalid DateStyle"))
        };
        let resolve_time_style = || -> Result<TimeStyle, minicbor::decode::Error> {
            let raw = style_raw.as_deref().ok_or_else(|| {
                minicbor::decode::Error::message("ValueFormat.time missing style")
            })?;
            TimeStyle::from_wire(raw)
                .ok_or_else(|| minicbor::decode::Error::message("invalid TimeStyle"))
        };
        let resolve_datetime_style = || -> Result<DateTimeStyle, minicbor::decode::Error> {
            let raw = style_raw.as_deref().ok_or_else(|| {
                minicbor::decode::Error::message("ValueFormat.datetime missing style")
            })?;
            DateTimeStyle::from_wire(raw)
                .ok_or_else(|| minicbor::decode::Error::message("invalid DateTimeStyle"))
        };
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("ValueFormat missing kind"))?;
        // Fields used by each variant. Anything else is rejected.
        let extras_present = |allowed_decimals: bool,
                              allowed_thousands: bool,
                              allowed_code: bool,
                              allowed_base: bool,
                              allowed_style: bool|
         -> Result<(), minicbor::decode::Error> {
            if !allowed_decimals && decimals.is_some()
                || !allowed_thousands && thousands_sep.is_some()
                || !allowed_code && code.is_some()
                || !allowed_base && base.is_some()
                || !allowed_style && style_raw.is_some()
            {
                return Err(minicbor::decode::Error::message(
                    "ValueFormat variant carries fields not allowed by its kind",
                ));
            }
            Ok(())
        };
        match kind.as_str() {
            "number" => {
                extras_present(true, true, false, false, false)?;
                Ok(ValueFormat::Number {
                    decimals: decimals.ok_or_else(|| {
                        minicbor::decode::Error::message("ValueFormat.number missing decimals")
                    })?,
                    thousands_sep: thousands_sep.ok_or_else(|| {
                        minicbor::decode::Error::message("ValueFormat.number missing thousands_sep")
                    })?,
                })
            }
            "currency" => {
                extras_present(false, false, true, false, false)?;
                Ok(ValueFormat::Currency {
                    code: code.ok_or_else(|| {
                        minicbor::decode::Error::message("ValueFormat.currency missing code")
                    })?,
                })
            }
            "percent" => {
                extras_present(true, false, false, false, false)?;
                Ok(ValueFormat::Percent {
                    decimals: decimals.ok_or_else(|| {
                        minicbor::decode::Error::message("ValueFormat.percent missing decimals")
                    })?,
                })
            }
            "bytes" => {
                extras_present(false, false, false, true, false)?;
                Ok(ValueFormat::Bytes {
                    base: base.ok_or_else(|| {
                        minicbor::decode::Error::message("ValueFormat.bytes missing base")
                    })?,
                })
            }
            "duration" => {
                extras_present(false, false, false, false, true)?;
                Ok(ValueFormat::Duration {
                    style: resolve_duration_style()?,
                })
            }
            "date" => {
                extras_present(false, false, false, false, true)?;
                Ok(ValueFormat::Date {
                    style: resolve_date_style()?,
                })
            }
            "time" => {
                extras_present(false, false, false, false, true)?;
                Ok(ValueFormat::Time {
                    style: resolve_time_style()?,
                })
            }
            "datetime" => {
                extras_present(false, false, false, false, true)?;
                Ok(ValueFormat::DateTime {
                    style: resolve_datetime_style()?,
                })
            }
            "relative" => {
                extras_present(false, false, false, false, false)?;
                Ok(ValueFormat::Relative)
            }
            "plain" => {
                extras_present(false, false, false, false, false)?;
                Ok(ValueFormat::Plain)
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown ValueFormat.kind: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: ValueFormat) {
        let mut buf1 = Vec::new();
        minicbor::encode(&v, &mut buf1).unwrap();
        let decoded: ValueFormat = minicbor::decode(&buf1).unwrap();
        assert_eq!(decoded, v);
        let mut buf2 = Vec::new();
        minicbor::encode(&decoded, &mut buf2).unwrap();
        assert_eq!(buf1, buf2);
    }

    #[test]
    fn roundtrip_all_variants() {
        roundtrip(ValueFormat::Number {
            decimals: 2,
            thousands_sep: true,
        });
        roundtrip(ValueFormat::Currency { code: "PLN".into() });
        roundtrip(ValueFormat::Percent { decimals: 1 });
        roundtrip(ValueFormat::Bytes {
            base: BytesBase::Binary,
        });
        roundtrip(ValueFormat::Duration {
            style: DurationStyle::Stopwatch,
        });
        roundtrip(ValueFormat::Date {
            style: DateStyle::Long,
        });
        roundtrip(ValueFormat::Time {
            style: TimeStyle::Medium,
        });
        roundtrip(ValueFormat::DateTime {
            style: DateTimeStyle::Full,
        });
        roundtrip(ValueFormat::Relative);
        roundtrip(ValueFormat::Plain);
    }

    #[test]
    fn currency_with_extra_decimals_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(3)
            .unwrap()
            .str("code")
            .unwrap()
            .str("EUR")
            .unwrap()
            .str("decimals")
            .unwrap()
            .u8(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("currency")
            .unwrap();
        let res: Result<ValueFormat, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn unknown_kind_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(1)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("magic")
            .unwrap();
        let res: Result<ValueFormat, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}
