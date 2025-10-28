use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Error, Fields, Type};

#[proc_macro_derive(ConfigBuilder)]
///Generates a builder pattern for a struct which implements:
/// - `Default` trait
/// - `new(Option<&std::path::Path>) -> std::io::Result<Self>` method
///
/// # Generated API
///
/// - `<StructName>Builder` struct with setter methods for each field
/// - `.build()` method that constructs the main struct, using defaults where needed
/// - `.from_file(path)` method that loads configuration from a file
/// - Getter methods for each field on the main struct
///
/// # Example
///
/// ```ignore
/// #[derive(Default, ConfigBuilder)]
/// struct AppConfig {
///     host: String,
///     port: u16,
///     debug: bool,
/// }
///
/// impl AppConfig {
///     fn new(path: Option<&std::path::Path>) -> std::io::Result<Self> {
///         // ...
///         Ok(Self::default())
///     }
/// }
///
/// // Usage
/// let config = AppConfig::builder()
///     .host("localhost")
///     .port(8080)
///     .debug(true)
///     .build();
///
/// // Or load from file
/// let config = AppConfig::builder()
///     .from_file(Some(Path::new("config.toml")))?
///     .port(9000)  // Can Override specific fields as well
///     .build();
/// ```
pub fn derive_config_builder(input: TokenStream) -> TokenStream {
    derive_config_builder_impl(input).unwrap_or_else(|err| err.to_compile_error().into())
}

fn derive_config_builder_impl(input: TokenStream) -> syn::Result<TokenStream> {
    let input = {
        let ts: proc_macro2::TokenStream = proc_macro2::TokenStream::from(input);
        syn::parse2::<DeriveInput>(ts)?
    };
    let struct_ident = input.ident.clone();
    let builder_ident = format_ident!("{}Builder", struct_ident);

    // Extract struct fields
    let fields = match input.data {
        Data::Struct(data_struct) => match data_struct.fields {
            Fields::Named(fields_named) => fields_named.named,
            _ => {
                return Err(Error::new_spanned(
                    struct_ident,
                    "ConfigBuilder only supports structs with named fields",
                ));
            }
        },
        _ => {
            return Err(Error::new_spanned(
                struct_ident,
                "ConfigBuilder only supports structs",
            ));
        }
    };

    // Builder fields - all wrapped in Option
    let builder_fields = fields.iter().map(|f| {
        let name = &f.ident;
        let ty = &f.ty;
        quote! {
            #name: Option<#ty>
        }
    });

    let setters = fields.iter().map(|f| {
        let name = &f.ident;
        let ty = &f.ty;

        if is_string_type(ty) {
            quote! {
                /// Sets the `#name` field.
                pub fn #name(mut self, #name: impl Into<String>) -> Self {
                    self.#name = Some(#name.into());
                    self
                }
            }
        } else {
            quote! {
                /// Sets the `#name` field.
                pub fn #name(mut self, #name: #ty) -> Self {
                    self.#name = Some(#name);
                    self
                }
            }
        }
    });

    // Build method -> merge with defaults
    let build_fields = fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            #name: self.#name.unwrap_or(default.#name)
        }
    });

    let getters = fields.iter().map(|f| {
        let name = &f.ident;
        let ty = &f.ty;

        if is_string_type(ty) {
            quote! {
                /// Returns the value of the `#name` field as a string slice.
                pub fn #name(&self) -> &str {
                    &self.#name
                }
            }
        } else if is_copy_type(ty) {
            quote! {
                /// Returns the value of the `#name` field.
                pub fn #name(&self) -> #ty {
                    self.#name
                }
            }
        } else {
            quote! {
                /// Returns a reference to the `#name` field.
                pub fn #name(&self) -> &#ty {
                    &self.#name
                }
            }
        }
    });

    // from_file assignments
    let from_file_assignments = fields.iter().map(|f| {
        let name = &f.ident;
        quote! {
            self.#name = Some(config.#name);
        }
    });

    let expanded = quote! {
        /// Builder struct generated for `#struct_ident`.
        /// All fields are optional during construction and will fall back to
        /// the struct's `Default` implementation if not set.
        #[derive(Default)]
        #[must_use = "Builders do nothing until you call build()"]
        pub struct #builder_ident {
            #(#builder_fields,)*
        }

        impl #builder_ident {
            /// Creates a new builder instance
            pub fn new() -> Self {
                Self::default()
            }

            #(#setters)*

            /// Loads configuration from a file and applies it to the builder.
            ///
            /// This method calls `#struct_ident::new(config_path)` to load the configuration,
            /// then sets all builder fields to the loaded values. You can still override
            /// specific fields by calling setters after `from_file()`.
            pub fn from_file(mut self, config_path: Option<&std::path::Path>) -> std::io::Result<Self> {
                let config = #struct_ident::new(config_path)?;
                #(#from_file_assignments)*
                Ok(self)
            }

            /// Builds a `#struct_ident` instance.
            /// Any fields not explicitly set will use the value from `#struct_ident::default()`.
            pub fn build(self) -> #struct_ident {
                let default = #struct_ident::default();
                #struct_ident {
                    #(#build_fields,)*
                }
            }
        }

        impl #struct_ident {
            /// Creates a new builder for this configuration struct.
            pub fn builder() -> #builder_ident {
                #builder_ident::default()
            }

            #(#getters)*
        }
    };

    Ok(TokenStream::from(expanded))
}

/// Checks if the type is `String`.
fn is_string_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        type_path
            .path
            .segments
            .last()
            .map(|seg| seg.ident == "String")
            .unwrap_or(false)
    } else {
        false
    }
}

/// Checks if the type is a primitive Copy type (integers, floats, bool).
fn is_copy_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        type_path
            .path
            .segments
            .last()
            .map(|seg| {
                matches!(
                    seg.ident.to_string().as_str(),
                    "u8" | "u16"
                        | "u32"
                        | "u64"
                        | "usize"
                        | "i8"
                        | "i16"
                        | "i32"
                        | "i64"
                        | "isize"
                        | "f32"
                        | "f64"
                        | "bool"
                )
            })
            .unwrap_or(false)
    } else {
        false
    }
}
