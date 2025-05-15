use proc_macro::TokenStream;
use proc_macro2::Ident;
use quote::quote;
use syn::{
    parse_macro_input, AngleBracketedGenericArguments, Data, DeriveInput, Fields, GenericArgument,
    Path, PathArguments, PathSegment, Type, TypePath,
};

/// checks if type is optional
// if type is optional return `Some(type)` else return `None`
#[rustfmt::skip]
fn ty_optional(ty: &Type) -> Option<&Ident> {
    if let Type::Path(TypePath { path: Path { segments, .. }, ..}) = ty {
        for PathSegment {ident, arguments} in segments {
            if ident != "Option" { return None; }
            if let PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. } ) = arguments {
                for arg in args {
                    if let GenericArgument::Type(Type::Path(TypePath { path: Path { segments, .. }, .. })) = arg {
                        // TODO: build the entire path (e.x.,std::string::String)?
                        return Some(&segments[segments.len() - 1].ident);
                    }
                }
            }
        }
    }
    None
}

fn create_methods(fields: &Fields) -> Vec<proc_macro2::TokenStream> {
    fields
        .into_iter()
        .map(|f| {
            let ident = &f.ident;
            let ty = &f.ty;
            if let Some(ident_ty) = ty_optional(ty) {
                return quote! {
                    pub(crate) fn #ident(&mut self, #ident: #ident_ty) -> &mut Self {
                        // TODO: can we do better?
                        self.#ident = Some(Some(#ident));
                        self
                    }
                };
            }
            quote! {
                pub(crate) fn #ident(&mut self, #ident: #ty) -> &mut Self {
                    self.#ident = Some(#ident);
                    self
                }
            }
        })
        .collect()
}

/// Derive macro generating boilerplate code involved in implementing the builder pattern.
#[proc_macro_derive(ConfigBuilder)]
pub fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let fields = if let Data::Struct(maker_config) = &input.data {
        &maker_config.fields
    } else {
        unimplemented!()
    };

    let ident = input.ident;

    let builder_ident = Ident::new(&format!("{}Builder", ident), ident.span());

    let builder_methods = create_methods(fields);

    let builder_fields = fields.into_iter().map(|f| {
        let ident = &f.ident;
        let ty = &f.ty;
        quote! { #ident: std::option::Option<#ty> }
    });

    // fields inside final config struct.
    let fields = fields.into_iter().map(|f| {
        let ident = &f.ident;
        let error = format!("{} is not set.", ident.clone().unwrap().to_string());
        let ty = &f.ty;
        if ty_optional(ty).is_some() {
            quote! { #ident: self.#ident.clone().and_then(|value| value) }
        } else {
            quote! { #ident: self.#ident.clone().expect(#error) }
        }
    });

    quote! {
        // TODO: derive all macros that are derived on the original struct dynamically?
        #[derive(Default)]
        pub struct #builder_ident {
            #(#builder_fields, )*
        }
        impl #builder_ident {
            pub(crate) fn build(&mut self) -> std::io::Result<#ident> {
                std::result::Result::Ok(
                    #ident {
                        #(#fields, )*
                    }
                )
            }
            #(#builder_methods)*
        }
        impl #ident {
            pub(crate) fn builder() -> #builder_ident {
                #builder_ident::default()
            }
        }
    }
    .into()
}
