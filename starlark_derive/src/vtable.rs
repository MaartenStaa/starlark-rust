/*
 * Copyright 2019 The Starlark in Rust Authors.
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use proc_macro2::TokenStream;
use quote::quote;
use quote::quote_spanned;
use syn::parse::ParseStream;
use syn::parse_macro_input;
use syn::spanned::Spanned;
use syn::FnArg;
use syn::ItemTrait;
use syn::Pat;
use syn::ReturnType;
use syn::TraitItem;
use syn::TraitItemFn;

struct Gen {
    starlark_value: ItemTrait,
}

struct VTableEntry {
    field: TokenStream,
    init: TokenStream,
    init_for_black_hole: TokenStream,
}

#[derive(Debug, Default)]
struct StarlarkInternalVTableAttrs {
    skip: bool,
}

impl syn::parse::Parse for StarlarkInternalVTableAttrs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ident = input.parse::<syn::Ident>()?;
        if ident == "skip" {
            Ok(StarlarkInternalVTableAttrs { skip: true })
        } else {
            Err(syn::Error::new(ident.span(), "unknown attribute"))
        }
    }
}

impl Gen {
    fn vtable_entry(&self, method: &TraitItemFn) -> syn::Result<VTableEntry> {
        let fn_name = &method.sig.ident;
        let fn_ret_type = &method.sig.output;
        let mut field_fn_param_types = Vec::new();
        let mut field_params_names = Vec::new();
        let mut field_init_args = Vec::new();
        for param in &method.sig.inputs {
            match param {
                FnArg::Receiver(_) => {
                    field_fn_param_types.push(quote_spanned! {method.sig.span()=>
                        crate::values::layout::vtable::StarlarkValueRawPtr
                    });
                    field_params_names.push(quote_spanned! {method.sig.span()=>
                        this
                    });
                    field_init_args.push(quote_spanned! {method.sig.span()=>
                        this.value_ref::<T>()
                    });
                }
                FnArg::Typed(p) => {
                    let name = match &*p.pat {
                        Pat::Ident(p) => p.ident.clone(),
                        _ => return Err(syn::Error::new(p.span(), "parameter must be identifier")),
                    };
                    let ty = &p.ty;
                    field_fn_param_types.push(quote_spanned! {method.sig.span()=>
                        #ty
                    });
                    field_params_names.push(quote_spanned! {method.sig.span()=>
                        #name
                    });
                    field_init_args.push(quote_spanned! {method.sig.span()=>
                        // We do `transmute` to get rid of lifetimes, see below.
                        std::mem::transmute(#name)
                    });
                }
            }
        }
        let field_init_param_pairs: Vec<TokenStream> = field_fn_param_types
            .iter()
            .zip(field_params_names.iter())
            .map(|(ty, name)| {
                quote_spanned! {method.sig.span()=>
                    #name: #ty
                }
            })
            .collect();
        let ret = match &method.sig.output {
            ReturnType::Default => quote! {},
            ReturnType::Type(_, ty) => {
                quote_spanned! {method.sig.span()=>
                    -> #ty
                }
            }
        };
        let field = quote_spanned! {method.sig.span()=>
            pub(crate) #fn_name: for<'a, 'v> fn(
                #(#field_fn_param_types),*
            ) #ret
        };
        let init = quote_spanned! {method.sig.span()=>
            #fn_name: {
                // It is important to put vtable entry into named function
                // instead of anonymous callback so function name is meaningful in profiler output.
                fn #fn_name<'a, 'v, 'v2, T: StarlarkValue<'v2>>(#(#field_init_param_pairs),*) #fn_ret_type {
                    unsafe {
                        // The problem is that it is concrete `'v` in
                        // ```
                        // struct StarlarkValueVTableGet<'v, T: StarlarkValue<'v>
                        // ```
                        // but we must generate vtable entry `for<'v>`.
                        //
                        // If Rust supported something like:
                        // ```
                        // struct StarlarkValueVTableGet<for<'v> T<'v>: StarlarkValue<'v>>
                        // ```
                        // it would be possible. But it doesn't. Hence all the transmutes.
                        std::mem::transmute(
                            T::#fn_name(#(#field_init_args),*)
                        )
                    }
                }
                #fn_name::<T>
            }
        };
        let init_for_black_hole = quote_spanned! {method.sig.span()=>
            #fn_name: |#(#field_params_names),*| {
                panic!("BlackHole")
            }
        };
        Ok(VTableEntry {
            field,
            init,
            init_for_black_hole,
        })
    }

    fn process_item_attrs(
        &self,
        attrs: &[syn::Attribute],
    ) -> syn::Result<(StarlarkInternalVTableAttrs, Vec<syn::Attribute>)> {
        let mut new_attrs = Vec::new();
        let mut item_attrs: Option<StarlarkInternalVTableAttrs> = None;
        for attr in attrs {
            if attr.path().is_ident("starlark_internal_vtable") {
                if item_attrs.is_some() {
                    return Err(syn::Error::new(attr.span(), "duplicate attribute"));
                }
                item_attrs = Some(attr.parse_args()?);
            } else {
                new_attrs.push(attr.clone());
            }
        }
        Ok((item_attrs.unwrap_or_default(), new_attrs))
    }

    fn gen_starlark_value_vtable(&self) -> syn::Result<TokenStream> {
        let mut fields = Vec::new();
        let mut inits = Vec::new();
        let mut init_black_holes = Vec::new();
        let mut starlark_value = self.starlark_value.clone();
        for item in &mut starlark_value.items {
            let m = match item {
                TraitItem::Fn(m) => m,
                _ => continue,
            };

            let (item_attrs, new_attrs) = self.process_item_attrs(&m.attrs)?;
            m.attrs = new_attrs;
            if item_attrs.skip {
                continue;
            }

            let VTableEntry {
                field,
                init,
                init_for_black_hole,
            } = self.vtable_entry(m)?;
            fields.push(field);
            inits.push(init);
            init_black_holes.push(init_for_black_hole);
        }

        Ok(quote_spanned! {
            self.starlark_value.span() =>

            #starlark_value

            pub(crate) struct StarlarkValueVTable {
                #(#fields),*
            }

            #[allow(clippy::all)]
            #[allow(unused_variables)]
            impl StarlarkValueVTable {
                pub(crate) const BLACK_HOLE: StarlarkValueVTable = StarlarkValueVTable {
                    #(#init_black_holes),*
                };
            }

            pub(crate) struct StarlarkValueVTableGet<'v, T: StarlarkValue<'v>>(std::marker::PhantomData<&'v T>);

            #[allow(clippy::all)]
            #[allow(unused_variables)]
            impl<'v, T: StarlarkValue<'v>> StarlarkValueVTableGet<'v, T> {
                pub(crate) const VTABLE: StarlarkValueVTable = StarlarkValueVTable {
                    #(#inits),*
                };
            }
        })
    }
}

/// Generates the `StarlarkValueVTable` and friends.
/// At the moment of writing this macro generated: P499428269,
/// output of `cargo expand -p starlark --lib values::traits`.
pub(crate) fn starlark_internal_vtable(
    _attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let starlark_value = parse_macro_input!(input as ItemTrait);

    let gen = Gen { starlark_value };
    let generated = match gen.gen_starlark_value_vtable() {
        Ok(generated) => generated,
        Err(error) => {
            return error.to_compile_error().into();
        }
    };

    proc_macro::TokenStream::from(quote! {
        #generated
    })
}
