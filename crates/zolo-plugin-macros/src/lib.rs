use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, ItemFn, Pat, ReturnType, Type, parse_macro_input};

#[proc_macro_attribute]
pub fn zolo_fn(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let fn_name = &input.sig.ident;
    let wrapper_name = syn::Ident::new(&format!("__zolo_wrapper_{fn_name}"), fn_name.span());
    let registry_name = syn::Ident::new(&format!("__zolo_registry_{fn_name}"), fn_name.span());

    let params: Vec<_> = input
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                if let Pat::Ident(ident) = pat_type.pat.as_ref() {
                    Some((ident.ident.clone(), pat_type.ty.as_ref().clone()))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    let param_count = params.len();

    let extractions: Vec<_> = params
        .iter()
        .enumerate()
        .map(|(i, (name, ty))| {
            let idx = syn::Index::from(i);
            extract_arg(name, ty, idx)
        })
        .collect();

    let param_names: Vec<_> = params.iter().map(|(name, _)| name).collect();

    let return_conv = match &input.sig.output {
        ReturnType::Default => quote! { Ok(zolo_plugin::PluginValue::Nil) },
        ReturnType::Type(_, ty) => convert_return(ty),
    };

    let param_defs: Vec<_> = params
        .iter()
        .map(|(name, ty)| {
            let name_str = name.to_string();
            let hint = type_to_hint(ty);
            quote! {
                zolo_plugin::ParamDef {
                    name: #name_str.to_string(),
                    type_hint: #hint,
                }
            }
        })
        .collect();

    let return_hint = match &input.sig.output {
        ReturnType::Default => quote! { zolo_plugin::TypeHint::Nil },
        ReturnType::Type(_, ty) => type_to_hint(ty),
    };

    let fn_name_str = fn_name.to_string();

    let output = quote! {
        #input

        fn #wrapper_name(args: &[zolo_plugin::PluginValue]) -> Result<zolo_plugin::PluginValue, zolo_plugin::PluginError> {
            if args.len() != #param_count {
                return Err(zolo_plugin::PluginError::ArityError {
                    expected: #param_count,
                    got: args.len(),
                    function: #fn_name_str.to_string(),
                });
            }
            #(#extractions)*
            let __result = #fn_name(#(#param_names),*);
            #return_conv
        }

        fn #registry_name() -> zolo_plugin::FunctionDef {
            zolo_plugin::FunctionDef {
                name: #fn_name_str.to_string(),
                func: #wrapper_name,
                sig: zolo_plugin::FnSignature {
                    params: vec![#(#param_defs),*],
                    returns: #return_hint,
                },
            }
        }
    };

    output.into()
}

fn extract_arg(name: &syn::Ident, ty: &Type, idx: syn::Index) -> proc_macro2::TokenStream {
    match type_string(ty).as_str() {
        "String" => quote! {
            let #name = args[#idx].as_string()?.to_string();
        },
        "f64" => quote! {
            let #name = args[#idx].as_f64()?;
        },
        "i64" => quote! {
            let #name = args[#idx].as_i64()?;
        },
        "bool" => quote! {
            let #name = args[#idx].as_bool()?;
        },
        _ => quote! {
            let #name = args[#idx].clone();
        },
    }
}

fn convert_return(ty: &Type) -> proc_macro2::TokenStream {
    match type_string(ty).as_str() {
        "String" => quote! { Ok(zolo_plugin::PluginValue::String(__result)) },
        "f64" => quote! { Ok(zolo_plugin::PluginValue::Number(__result)) },
        "i64" => quote! { Ok(zolo_plugin::PluginValue::Integer(__result)) },
        "bool" => quote! { Ok(zolo_plugin::PluginValue::Bool(__result)) },
        _ => quote! { Ok(zolo_plugin::PluginValue::from(__result)) },
    }
}

fn type_to_hint(ty: &Type) -> proc_macro2::TokenStream {
    match type_string(ty).as_str() {
        "String" | "&str" => quote! { zolo_plugin::TypeHint::String },
        "f64" => quote! { zolo_plugin::TypeHint::Number },
        "i64" | "i32" => quote! { zolo_plugin::TypeHint::Integer },
        "bool" => quote! { zolo_plugin::TypeHint::Bool },
        _ => quote! { zolo_plugin::TypeHint::Any },
    }
}

fn type_string(ty: &Type) -> String {
    if let Type::Path(p) = ty {
        p.path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    }
}
