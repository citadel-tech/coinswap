use proc_macro::TokenStream;
use proc_macro2::Ident;
use quote::{quote, ToTokens};
use syn::{
    parse_macro_input, AngleBracketedGenericArguments, Data, DeriveInput, GenericArgument,
    PathArguments, PathSegment, Type, TypePath,
};

/// Checks if type is optional and then returns the type.
fn ty_optional(ty: &Type) -> Option<proc_macro2::TokenStream> {
    if let Type::Path(TypePath { path, .. }) = ty {
        for PathSegment { ident, arguments } in &path.segments {
            if ident != "Option" {
                continue;
            }
            if let PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. }) =
                arguments
            {
                for arg in args {
                    if let GenericArgument::Type(Type::Path(TypePath { path, .. })) = arg {
                        let segments = &path.segments;
                        // build the entire path to type
                        let mut path = proc_macro2::TokenStream::new();
                        path.extend(segments.iter().enumerate().map(|(i, segment)| {
                            let ident = &segment.ident;
                            if i == segments.len() - 1 {
                                return quote! { #ident };
                            }
                            quote! { #ident:: }
                        }));
                        return path.into();
                    }
                }
            }
        }
    };
    None
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

    let builder_methods = fields.into_iter().map(|f| {
        let ident = &f.ident;
        let ty = ty_optional(&f.ty).unwrap_or(f.ty.to_token_stream());
        quote! {
            pub(crate) fn #ident(&mut self, #ident: #ty) -> &mut Self {
                self.#ident = std::option::Option::Some(#ident);
                self
            }
        }
    });

    let builder_fields = fields.into_iter().map(|f| {
        let ident = &f.ident;
        let ty = &f.ty;
        if ty_optional(ty).is_some() {
            return quote! { #ident: #ty };
        }
        quote! { #ident: std::option::Option<#ty> }
    });

    // fields inside final config struct.
    let fields = fields.into_iter().map(|f| {
        let ident = &f.ident;
        let error = format!("{} is not set.", ident.clone().unwrap().to_string());
        if ty_optional(&f.ty).is_some() {
            return quote! { #ident: self.#ident.clone() };
        }
        quote! { #ident: self.#ident.clone().expect(#error) }
    });

    quote! {
        // TODO: derive all macros that are derived on the original struct dynamically
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
