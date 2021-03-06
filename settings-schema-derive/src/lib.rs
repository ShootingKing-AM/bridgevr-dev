use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, ToTokens};
use std::string::ToString;
use syn::{
    Attribute, Data, DeriveInput, Error, Fields, FieldsNamed, GenericArgument, Ident, Lit, Meta,
    NestedMeta, PathArguments, Type,
};

fn error<T, TT: ToTokens>(message: &str, tokens: TT) -> Result<T, TokenStream> {
    Err(
        Error::new_spanned(tokens, format!("[SettingsSchema] {}", message))
            .to_compile_error()
            .into(),
    )
}

fn schema_fn_ident(ty: &Ident) -> Ident {
    // todo: use proper snake_case transformation
    Ident::new(
        &format!("{}_schema", ty.to_string().to_lowercase()),
        ty.span(),
    )
}

fn suffix_ident(ty_ident: &Ident, suffix: &str) -> Ident {
    Ident::new(
        &format!("{}{}", ty_ident.to_string(), suffix),
        ty_ident.span(),
    )
}

fn get_only_type_argument(arguments: &PathArguments) -> &Type {
    if let PathArguments::AngleBracketed(args_block) = &arguments {
        if let GenericArgument::Type(ty) = args_block.args.first().unwrap() {
            return ty;
        }
    }
    // Fail cases are already handled by the compiler
    unreachable!()
}

struct SchemaAttributes {
    advanced: bool,
    min: Option<Lit>,
    max: Option<Lit>,
    step: Option<Lit>,
    gui: Option<Lit>,
}

fn schema_attributes(attrs: Vec<Attribute>) -> Result<SchemaAttributes, TokenStream> {
    let mut advanced = false;
    let mut min = None;
    let mut max = None;
    let mut step = None;
    let mut gui = None;
    for attr in schema_attrs(attrs) {
        let parsed_attr = attr
            .parse_meta()
            .map_err(|e| e.to_compile_error().into_token_stream())?;
        match parsed_attr {
            Meta::List(args_list) => {
                for arg in args_list.nested {
                    if let NestedMeta::Meta(meta_arg) = arg {
                        match meta_arg {
                            Meta::Path(path_arg) => {
                                if let Some(arg_ident) = path_arg.get_ident() {
                                    if arg_ident == "advanced" {
                                        advanced = true;
                                    } else {
                                        return error(
                                            "Unknown identifier or missing value",
                                            path_arg,
                                        );
                                    }
                                } else {
                                    return error("Expected identifier", path_arg);
                                }
                            }
                            Meta::NameValue(name_value_arg) => {
                                if let Some(arg_ident) = name_value_arg.path.get_ident() {
                                    match arg_ident.to_string().as_str() {
                                        "min" => min = Some(name_value_arg.lit),
                                        "max" => max = Some(name_value_arg.lit),
                                        "step" => step = Some(name_value_arg.lit),
                                        "gui" => gui = Some(name_value_arg.lit),
                                        _ => return error("Unknown argument name", arg_ident),
                                    }
                                } else {
                                    return error("Expected identifier", name_value_arg.path);
                                }
                            }
                            _ => return error("Nested arguments not supported", meta_arg),
                        }
                    } else {
                        return error("Unexpected literal", arg);
                    }
                }
            }
            _ => return error("Expected arguments", parsed_attr),
        }
    }
    Ok(SchemaAttributes {
        advanced,
        min,
        max,
        step,
        gui,
    })
}

struct TypeSchema {
    default_ty_ts: TokenStream2,
    schema_code_ts: TokenStream2,
}

fn bool_type_schema(schema_attrs: SchemaAttributes) -> Result<TokenStream2, TokenStream> {
    let maybe_invalid_arg = if let Some(min) = schema_attrs.min {
        Some(min)
    } else if let Some(max) = schema_attrs.max {
        Some(max)
    } else if let Some(step) = schema_attrs.step {
        Some(step)
    } else if let Some(gui) = schema_attrs.gui {
        Some(gui)
    } else {
        None
    };
    if let Some(arg) = maybe_invalid_arg {
        error("Unexpected argument for bool type", arg)?;
    }

    let advanced = schema_attrs.advanced;
    Ok(quote! {
        settings_schema::SchemaNode {
            advanced: #advanced,
            node_type: settings_schema::SchemaNodeType::Boolean { default }
        }
    })
}

fn integer_literal_tokens(literal: Lit) -> Result<TokenStream2, TokenStream> {
    if let Lit::Int(lit_int) = literal {
        Ok(quote!(#lit_int))
    } else {
        error("Expected integer literal", literal)
    }
}

fn maybe_float_literal(literal: Option<Lit>) -> Result<TokenStream2, TokenStream> {
    if let Some(literal) = literal {
        if let Lit::Float(lit_float) = literal {
            Ok(quote!(Some(#lit_float as _)))
        } else {
            error("Expected float literal", literal)
        }
    } else {
        Ok(quote!(None))
    }
}

fn maybe_numeric_gui(literal: Option<Lit>) -> Result<TokenStream2, TokenStream> {
    if let Some(literal) = literal {
        if let Lit::Str(lit_str) = literal {
            let lit_val = lit_str.value();
            if matches!(lit_val.as_str(), "TextBox" | "UpDown" | "Slider") {
                let ident = Ident::new(&lit_val, lit_str.span());
                Ok(quote!(Some(settings_schema::NumericGuiType::#ident)))
            } else {
                error(r#"Expected "TextBox", "UpDown" or "Slider""#, lit_str)
            }
        } else {
            error("Expected string literal", literal)
        }
    } else {
        Ok(quote!(None))
    }
}

fn integer_type_schema(
    ty_ident: &Ident,
    schema_attrs: SchemaAttributes,
) -> Result<TokenStream2, TokenStream> {
    let min_ts = if let Some(literal) = schema_attrs.min {
        integer_literal_tokens(literal)?
    } else {
        quote!(std::#ty_ident::MIN)
    };
    let max_ts = if let Some(literal) = schema_attrs.max {
        integer_literal_tokens(literal)?
    } else {
        quote!(std::#ty_ident::MAX)
    };
    let step_ts = if let Some(literal) = schema_attrs.step {
        integer_literal_tokens(literal)?
    } else {
        quote!(1)
    };
    let gui_ts = maybe_numeric_gui(schema_attrs.gui)?;

    let advanced = schema_attrs.advanced;
    Ok(quote! {{
        // use explicit type to catch overflows at compile time
        let min: #ty_ident = #min_ts;
        let max: #ty_ident = #max_ts;
        let step: #ty_ident = #step_ts;
        settings_schema::SchemaNode {
            advanced: #advanced,
            node_type: settings_schema::SchemaNodeType::Integer {
                default: default as _,
                min: min as _,
                max: max as _,
                step: step as _,
                gui: #gui_ts,
            }
        }
    }})
}

fn float_type_schema(schema_attrs: SchemaAttributes) -> Result<TokenStream2, TokenStream> {
    let min_ts = maybe_float_literal(schema_attrs.min)?;
    let max_ts = maybe_float_literal(schema_attrs.max)?;
    let step_ts = maybe_float_literal(schema_attrs.step)?;
    let gui_ts = maybe_numeric_gui(schema_attrs.gui)?;

    let advanced = schema_attrs.advanced;
    Ok(quote! {
        settings_schema::SchemaNode {
            advanced: #advanced,
            node_type: settings_schema::SchemaNodeType::Float {
                default: default as _,
                min: #min_ts,
                max: #max_ts,
                step: #step_ts,
                gui: #gui_ts,
            }
        }
    })
}

fn string_type_schema(schema_attrs: SchemaAttributes) -> Result<TokenStream2, TokenStream> {
    let maybe_invalid_arg = if let Some(min) = schema_attrs.min {
        Some(min)
    } else if let Some(max) = schema_attrs.max {
        Some(max)
    } else if let Some(step) = schema_attrs.step {
        Some(step)
    } else if let Some(gui) = schema_attrs.gui {
        Some(gui)
    } else {
        None
    };
    if let Some(arg) = maybe_invalid_arg {
        error("Unexpected argument for String type", arg)?;
    }

    let advanced = schema_attrs.advanced;
    Ok(quote! {
        settings_schema::SchemaNode {
            advanced: #advanced,
            node_type: settings_schema::SchemaNodeType::Text { default }
        }
    })
}

fn custom_leaf_type_schema(
    ty_ident: &Ident,
    schema_attrs: SchemaAttributes,
) -> Result<TokenStream2, TokenStream> {
    let maybe_invalid_arg = if let Some(min) = schema_attrs.min {
        Some(min)
    } else if let Some(max) = schema_attrs.max {
        Some(max)
    } else if let Some(step) = schema_attrs.step {
        Some(step)
    } else if let Some(gui) = schema_attrs.gui {
        Some(gui)
    } else {
        None
    };
    if let Some(arg) = maybe_invalid_arg {
        error("Unexpected argument for custom type", arg)?;
    }

    let leaf_schema_fn_ident = schema_fn_ident(ty_ident);
    let advanced = schema_attrs.advanced;
    Ok(quote! {{
        let mut default = #leaf_schema_fn_ident(default);
        default.advanced = #advanced;
        default
    }})
}

fn type_schema(ty: &Type, schema_attrs: SchemaAttributes) -> Result<TypeSchema, TokenStream> {
    let advanced = schema_attrs.advanced;
    match &ty {
        Type::Array(ty_array) => {
            let len = &ty_array.len;
            let TypeSchema {
                default_ty_ts,
                schema_code_ts,
            } = type_schema(&*ty_array.elem, schema_attrs)?;
            Ok(TypeSchema {
                default_ty_ts: quote!([#default_ty_ts; #len]),
                schema_code_ts: quote! {{
                    let length = #len;
                    // Note: for arrays, into_iter() behaves like iter(), because of a
                    // implementation complication in the std library. Blocked by const generics.
                    // For now clone() is necessary.
                    let content = default.iter().map(|default| {
                        let default = default.clone();
                        #schema_code_ts
                    }).collect::<Vec<_>>();

                    settings_schema::SchemaNode {
                        advanced: #advanced,
                        node_type: settings_schema::SchemaNodeType::Array(content),
                    }
                }},
            })
        }
        Type::Path(ty_path) => {
            let ty_last = ty_path.path.segments.last().unwrap();
            let ty_ident = &ty_last.ident;
            if matches!(ty_last.arguments, PathArguments::None) {
                let mut custom_default_ty_ts = None;
                let schema_code_ts = match ty_ident.to_string().as_str() {
                    "bool" => bool_type_schema(schema_attrs)?,
                    "i8" | "u8" | "i16" | "u16" | "i32" | "u32" | "i64" | "u64" => {
                        integer_type_schema(ty_ident, schema_attrs)?
                    }
                    "f32" | "f64" => float_type_schema(schema_attrs)?,
                    "String" => string_type_schema(schema_attrs)?,
                    _ => {
                        custom_default_ty_ts =
                            Some(suffix_ident(&ty_ident, "Default").to_token_stream());
                        custom_leaf_type_schema(ty_ident, schema_attrs)?
                    }
                };
                Ok(TypeSchema {
                    default_ty_ts: if let Some(tokens) = custom_default_ty_ts {
                        tokens
                    } else {
                        ty_ident.to_token_stream()
                    },
                    schema_code_ts,
                })
            } else if ty_ident == "Option" {
                let TypeSchema {
                    default_ty_ts,
                    schema_code_ts,
                } = type_schema(get_only_type_argument(&ty_last.arguments), schema_attrs)?;
                Ok(TypeSchema {
                    default_ty_ts: quote!(settings_schema::OptionalDefault<#default_ty_ts>),
                    schema_code_ts: quote! {{
                        let default_set = default.set;
                        let default = default.content;
                        let content = Box::new(#schema_code_ts);
                        settings_schema::SchemaNode {
                            advanced: #advanced,
                            node_type: settings_schema::SchemaNodeType::Optional { default_set, content }
                        }
                    }},
                })
            } else if ty_ident == "Switch" {
                let TypeSchema {
                    default_ty_ts,
                    schema_code_ts,
                } = type_schema(get_only_type_argument(&ty_last.arguments), schema_attrs)?;
                Ok(TypeSchema {
                    default_ty_ts: quote!(settings_schema::SwitchDefault<#default_ty_ts>),
                    schema_code_ts: quote! {{
                        let default_enabled = default.enabled;
                        let default = default.content;
                        let content = Box::new(#schema_code_ts);
                        settings_schema::SchemaNode {
                            advanced: #advanced,
                            node_type: settings_schema::SchemaNodeType::Switch { default_enabled, content }
                        }
                    }},
                })
            } else if ty_ident == "Vec" {
                let ty = get_only_type_argument(&ty_last.arguments);
                if let Type::Tuple(ty_tuple) = ty {
                    if ty_tuple.elems.len() != 2 {
                        error("Expected two arguments", &ty_tuple.elems)
                    } else if ty_tuple.elems[0].to_token_stream().to_string() != "String" {
                        error("First argument must be a `String`", &ty_tuple.elems)
                    } else {
                        let ty = &ty_tuple.elems[1];
                        let TypeSchema {
                            default_ty_ts,
                            schema_code_ts,
                        } = type_schema(ty, schema_attrs)?;
                        Ok(TypeSchema {
                            default_ty_ts: quote!(settings_schema::DictionaryDefault<#default_ty_ts, #ty>),
                            schema_code_ts: quote! {{
                                let default_content =
                                    serde_json::to_value(default.default).unwrap();
                                let default_key = default.key;
                                let default = default.value;
                                let default_value = Box::new(#schema_code_ts);
                                settings_schema::SchemaNode {
                                    advanced: #advanced,
                                    node_type: settings_schema::SchemaNodeType::Dictionary {
                                        default_key,
                                        default_value,
                                        default: default_content }
                                }
                            }},
                        })
                    }
                } else {
                    let TypeSchema {
                        default_ty_ts,
                        schema_code_ts,
                    } = type_schema(ty, schema_attrs)?;
                    Ok(TypeSchema {
                        default_ty_ts: quote!(settings_schema::VectorDefault<#default_ty_ts, #ty>),
                        schema_code_ts: quote! {{
                            let default_content =
                                serde_json::to_value(default.default).unwrap();
                            let default = default.element;
                            let default_element = Box::new(#schema_code_ts);
                            settings_schema::SchemaNode {
                                advanced: #advanced,
                                node_type: settings_schema::SchemaNodeType::Vector {
                                    default_element,
                                    default: default_content
                                }
                            }
                        }},
                    })
                }
            } else {
                error("Generics are supported only for Option, Switch, Vec", &ty)
            }
        }
        _ => error("Unsupported type", &ty),
    }
}

fn schema_attrs(attrs: Vec<Attribute>) -> Vec<Attribute> {
    attrs
        .into_iter()
        .filter(|attr| {
            if let Some(attr_ident) = attr.path.get_ident() {
                attr_ident == "schema"
            } else {
                false
            }
        })
        .collect()
}

struct NamedFieldsData {
    idents: Vec<Ident>,
    tys_ts: Vec<TokenStream2>,
    schema_code_ts: TokenStream2,
}

fn schema_named_fields(fields_block: FieldsNamed) -> Result<NamedFieldsData, TokenStream> {
    let mut idents = vec![];
    let mut tys_ts = vec![];
    let mut schema_values_ts = vec![];
    for field in fields_block.named {
        let schema_attrs = schema_attributes(field.attrs)?;
        let TypeSchema {
            default_ty_ts,
            schema_code_ts,
        } = type_schema(&field.ty, schema_attrs)?;
        idents.push(field.ident.unwrap());
        tys_ts.push(default_ty_ts);
        schema_values_ts.push(schema_code_ts);
    }

    let schema_keys = idents.iter().map(ToString::to_string);
    let schema_code_ts = quote! {{
        let mut entries = vec![];
        #(
            entries.push({
                let default = default.#idents;
                (#schema_keys.into(), #schema_values_ts)
            });
        )*
        settings_schema::SchemaNode {
            advanced: false,
            node_type: settings_schema::SchemaNodeType::Section { entries }
        }
    }};

    Ok(NamedFieldsData {
        idents,
        tys_ts,
        schema_code_ts,
    })
}

fn schema(input: DeriveInput) -> Result<TokenStream2, TokenStream> {
    let vis = input.vis;
    let default_ty_ident = suffix_ident(&input.ident, "Default");
    let schema_fn_ident = schema_fn_ident(&input.ident);

    if !input.generics.params.is_empty() {
        return error("Generics not supported", &input.generics);
    }

    let schema_attrs = schema_attrs(input.attrs);
    if !schema_attrs.is_empty() {
        return error(
            "`schema` attribute supported only on fields and variants",
            &schema_attrs[0],
        );
    }

    let mut field_idents = vec![];
    let mut field_tys_ts = vec![];
    let schema_root_code_ts;
    let mut maybe_aux_objects_ts = None;
    match input.data {
        Data::Struct(data_struct) => {
            match data_struct.fields {
                Fields::Named(fields_block) => {
                    let fields_data = schema_named_fields(fields_block)?;
                    field_idents = fields_data.idents;
                    field_tys_ts = fields_data.tys_ts;
                    schema_root_code_ts = fields_data.schema_code_ts;
                }
                Fields::Unnamed(fields_block) => {
                    return error("Unnamed fields not supported", fields_block)
                }
                Fields::Unit => return error("Unit structs not supported", default_ty_ident),
            };
        }
        Data::Enum(data_enum) => {
            let variant_ty_ident = suffix_ident(&input.ident, "DefaultVariant");

            let mut variant_idents = vec![];
            let mut variant_strings = vec![];
            let mut variant_aux_objects_ts = vec![];
            let mut schema_variants_ts = vec![];
            for variant in data_enum.variants {
                let schema_attrs = schema_attributes(variant.attrs)?;
                let variant_ident = variant.ident;
                let variant_string = variant_ident.to_string();
                match variant.fields {
                    Fields::Named(fields_block) => {
                        let variant_fields_data = schema_named_fields(fields_block)?;
                        let variant_field_idents = variant_fields_data.idents;
                        let variant_field_tys_ts = variant_fields_data.tys_ts;
                        let schema_variant_fields_code_ts = variant_fields_data.schema_code_ts;

                        let variant_default_ty_ident =
                            suffix_ident(&input.ident, &format!("{}Default", variant_string));

                        field_idents.push(variant_ident.clone());
                        field_tys_ts.push(variant_default_ty_ident.to_token_stream());
                        schema_variants_ts.push(quote!{{
                            let default = default.#variant_ident;
                            Some(#schema_variant_fields_code_ts)
                        }});

                        variant_aux_objects_ts.push(quote! {
                            #[derive(Clone)]
                            #vis struct #variant_default_ty_ident {
                                pub #(#variant_field_idents: #variant_field_tys_ts,)*
                            }
                        });
                    }
                    Fields::Unnamed(fields_block) => {
                        if fields_block.unnamed.len() != 1 {
                            return error("Only one unnamed field is suppoted", fields_block);
                        }
                        field_idents.push(variant_ident.clone());

                        let TypeSchema {
                            default_ty_ts,
                            schema_code_ts,
                        } = type_schema(&fields_block.unnamed[0].ty, schema_attrs)?;
                        field_tys_ts.push(default_ty_ts);

                        schema_variants_ts.push(quote!{{
                            let default = default.#variant_ident;
                            Some(#schema_code_ts)
                        }});
                    }
                    Fields::Unit => {
                        schema_variants_ts.push(quote!(None));
                    }
                }

                variant_idents.push(variant_ident);
                variant_strings.push(variant_string);
            }

            maybe_aux_objects_ts = Some(quote! {
                #(#variant_aux_objects_ts)*

                #[derive(serde::Serialize, serde::Deserialize, Clone)]
                #vis enum #variant_ty_ident {
                    #(#variant_idents,)*
                }
            });

            field_idents.push(Ident::new("variant", Span::call_site()));
            field_tys_ts.push(variant_ty_ident.to_token_stream());

            schema_root_code_ts = quote! {{
                let mut variants = vec![];
                #(variants.push((#variant_strings.into(), #schema_variants_ts));)*
                let default = serde_json::to_value(default.variant)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .into();

                settings_schema::SchemaNode {
                    advanced: false,
                    node_type: settings_schema::SchemaNodeType::Choice {
                        variants,
                        default,
                    }
                }
            }}
        }
        Data::Union(data_union) => return error("Unions not supported", data_union.union_token),
    }

    Ok(quote! {
        #maybe_aux_objects_ts

        #[allow(non_snake_case)]
        #[derive(Clone)]
        #vis struct #default_ty_ident {
            #(pub #field_idents: #field_tys_ts,)*
        }

        #vis fn #schema_fn_ident(default: #default_ty_ident) -> settings_schema::SchemaNode {
            #schema_root_code_ts
        }
    })
}

// Add this derive attribute to a struct or enum to allow adding GUI related metadata and to create
// a settings schema map.
#[proc_macro_derive(SettingsSchema, attributes(schema))]
pub fn create_settings_schema_fn_and_default_ty(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);

    match schema(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e,
    }
}
