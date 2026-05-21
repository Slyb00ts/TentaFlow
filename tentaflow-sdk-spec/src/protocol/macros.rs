// =============================================================================
// File: protocol/macros.rs — internal CBOR encoding helper macros
// Purpose: deduplicate string-discriminated enum codecs used across control
// and UI modules. Macros emit Encode/Decode impls that map variants to/from
// tstr wire form and reject unknown values.
// =============================================================================

/// Defines a Rust enum whose CBOR wire form is a tstr.
///
/// Encode emits the literal mapped to each variant; decode rejects unknown
/// variants with a descriptive error.
#[macro_export]
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident = $literal:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $literal,)+
                }
            }

            pub fn from_wire(s: &str) -> Option<Self> {
                match s {
                    $($literal => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl<C> ::minicbor::Encode<C> for $name {
            fn encode<W: ::minicbor::encode::Write>(
                &self,
                e: &mut ::minicbor::Encoder<W>,
                _ctx: &mut C,
            ) -> ::core::result::Result<(), ::minicbor::encode::Error<W::Error>> {
                e.str(self.as_str())?;
                Ok(())
            }
        }

        impl<'b, C> ::minicbor::Decode<'b, C> for $name {
            fn decode(
                d: &mut ::minicbor::Decoder<'b>,
                _ctx: &mut C,
            ) -> ::core::result::Result<Self, ::minicbor::decode::Error> {
                let s = d.str()?;
                Self::from_wire(s).ok_or_else(|| {
                    ::minicbor::decode::Error::message(concat!(
                        "unknown ",
                        stringify!($name),
                        " variant"
                    ))
                })
            }
        }
    };
}
