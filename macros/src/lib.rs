//! aurora 过程宏。
//!
//! Rust 规定过程宏必须定义在独立的 `proc-macro` crate 中，因此本仓库是
//! 「主 crate + 宏 crate」的最小 workspace（见 `src/macros.rs`）。
//!
//! # 为什么是属性宏而不是派生宏
//!
//! `sea_orm::DeriveEntityModel` 会把 `#[sea_orm(table_name = "...")]` 直接写进
//! `EntityName::table_name -> &'static str`，运行时改不了——这正是
//! `[database].table_prefix` 无法只靠配置实现的原因。
//!
//! 派生宏只能**追加**代码，无法改写被派生的结构体本身（也就无法去掉
//! `DeriveEntityModel`、无法拦住 `table_name`）。属性宏替换整个 item，恰好能：
//!
//! 1. 给结构体补上 `DeriveEntityModel`，但**不传** `table_name`——这样它只生成
//!    `Model` / `Column` / `PrimaryKey`（`sea-orm-macros` 的 `entity_model.rs` 中
//!    `entity_def` 仅在存在表名时生成 `Entity`），列解析完全复用官方实现；
//! 2. 自行生成 `Entity`（仍是官方 `DeriveEntity`）并实现 `EntityName`，
//!    表名指向 `aurora::db::prefix::table(逻辑表名)` 做运行时解析。
//!
//! 因为 `IdenStatic::as_str` 本身就委托给 `EntityName::table_name`，
//! 改写这一个方法即可让查询、插入、更新、关联 join 全部带上前缀。

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Data, DeriveInput, Ident, LitStr, Meta, Path, Token, parse_macro_input, parse_quote,
};

/// 表前缀感知的实体声明宏（替代 `sea_orm::DeriveEntityModel` + `table_name`）。
///
/// ```ignore
/// use aurora::db::aurora_entity;
///
/// #[aurora_entity(table = "users")] // 逻辑表名（不含前缀）
/// #[derive(Clone, Debug, PartialEq)]
/// pub struct Model {
///     #[sea_orm(primary_key)]
///     pub id: i64,
/// }
/// ```
///
/// `[database].table_prefix = "book"` 时该实体访问的物理表是 `book_users`；
/// 前缀为空时与原表名一致。`Model` / `Column` / `PrimaryKey` / `ActiveModel`
/// 的生成方式与 `DeriveEntityModel` 完全一致，字段级 `#[sea_orm(...)]` 原样生效。
///
/// 支持参数：
/// - `table = "逻辑表名"`（必填）
/// - `schema = "schema 名"`（可选，静态）
/// - `comment = "表注释"`（可选）
///
/// # 依赖名约束
///
/// 生成的代码使用 `::aurora::...` 绝对路径，因此本 crate 在 `Cargo.toml` 里必须
/// 以 `aurora` 这个名字可见。重命名依赖（`aurora = { package = ..., ... }`）时，
/// 请补一行 `extern crate aurora_renamed as aurora;` 保持路径可解析。
#[proc_macro_attribute]
pub fn aurora_entity(args: TokenStream, item: TokenStream) -> TokenStream {
    let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
    let args = match parser.parse(args) {
        Ok(args) => args,
        Err(error) => return error.into_compile_error().into(),
    };
    let input = parse_macro_input!(item as DeriveInput);
    expand(args, input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// `#[aurora_entity(...)]` 解析结果。
#[derive(Default)]
struct LogicalName {
    table: Option<LitStr>,
    schema: Option<LitStr>,
    comment: Option<LitStr>,
    /// `#[sea_orm(model_ex = ..., active_model_ex = ...)]`：需要原样转给生成的 `Entity`。
    entity_extra: Vec<(Path, TokenStream2)>,
}

fn expand(args: Punctuated<Meta, Token![,]>, input: DeriveInput) -> syn::Result<TokenStream2> {
    if input.ident != "Model" {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`aurora_entity` 只能用于名为 `Model` 的结构体（与 SeaORM 的 `DeriveEntityModel` 约定一致）",
        ));
    }
    if !matches!(input.data, Data::Struct(_)) {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`aurora_entity` 只能用于结构体",
        ));
    }

    let logical = collect_args(args, &input.attrs)?;
    let Some(table) = logical.table else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "缺少 `table = \"...\"`：宏需要逻辑表名（不含前缀）才能在运行时拼出物理表名，\
             例如 `#[aurora_entity(table = \"users\")]`",
        ));
    };
    validate_identifier(&table)?;

    // 补上 `DeriveEntityModel`，其余属性（字段级 `#[sea_orm(...)]`、`#[serde(...)]`）原样保留。
    let mut item = input.clone();
    let mut derives: Vec<Path> = Vec::new();
    let mut attrs: Vec<Attribute> = Vec::new();
    for attr in &input.attrs {
        if attr.path().is_ident("derive") {
            let paths: Punctuated<Path, Token![,]> =
                attr.parse_args_with(Punctuated::parse_terminated)?;
            for path in paths {
                if is_named(&path, "DeriveEntityModel") {
                    return Err(syn::Error::new_spanned(
                        &path,
                        "`aurora_entity` 已内置 `DeriveEntityModel`：请删除重复的派生（两者都会生成 `Entity`）",
                    ));
                }
                derives.push(path);
            }
            continue;
        }
        attrs.push(attr.clone());
    }
    let derive_entity_model: Path = parse_quote!(::aurora::db::sea_orm::prelude::DeriveEntityModel);
    derives.push(derive_entity_model);
    let mut derive_attr: Attribute = parse_quote!(#[derive()]);
    if let syn::Meta::List(list) = &mut derive_attr.meta {
        let tokens = derives.iter().map(ToTokens::to_token_stream);
        list.tokens = quote!(#(#tokens),*);
    }
    attrs.push(derive_attr);
    item.attrs = attrs;

    let vis = &input.vis;
    let entity_extra = if logical.entity_extra.is_empty() {
        TokenStream2::new()
    } else {
        let entries = logical
            .entity_extra
            .iter()
            .map(|(path, value)| quote!(#path = #value));
        quote!(#[sea_orm(#(#entries),*)])
    };
    let schema_fn = logical.schema.as_ref().map(|schema| {
        quote! {
            fn schema_name(&self) -> ::std::option::Option<&str> {
                ::std::option::Option::Some(#schema)
            }
        }
    });
    let comment_fn = logical.comment.as_ref().map(|comment| {
        quote! {
            fn comment(&self) -> ::std::option::Option<&str> {
                ::std::option::Option::Some(#comment)
            }
        }
    });

    Ok(quote! {
        #item

        #[doc = " 由 `aurora_entity` 生成：表名在运行时按 `[database].table_prefix` 解析。"]
        #[derive(Copy, Clone, Debug, Default, ::aurora::db::sea_orm::prelude::DeriveEntity)]
        #entity_extra
        #vis struct Entity;

        impl ::aurora::db::sea_orm::entity::EntityName for Entity {
            fn table_name(&self) -> &'static str {
                ::aurora::db::prefix::table(#table)
            }

            #schema_fn
            #comment_fn
        }
    })
}

/// 解析属性参数，并对结构体级 `#[sea_orm(...)]` 做冲突检查。
fn collect_args(
    args: Punctuated<Meta, Token![,]>,
    attrs: &[Attribute],
) -> syn::Result<LogicalName> {
    let mut logical = LogicalName::default();

    for meta in args {
        let slot = if meta.path().is_ident("table") {
            &mut logical.table
        } else if meta.path().is_ident("schema") {
            &mut logical.schema
        } else if meta.path().is_ident("comment") {
            &mut logical.comment
        } else {
            return Err(syn::Error::new_spanned(
                &meta,
                "未知参数：`aurora_entity` 仅支持 `table` / `schema` / `comment`",
            ));
        };
        if slot.is_some() {
            return Err(syn::Error::new_spanned(&meta, "该参数重复出现"));
        }
        let Meta::NameValue(name_value) = &meta else {
            return Err(syn::Error::new_spanned(
                &meta,
                "参数需要写成 `key = \"value\"` 形式",
            ));
        };
        *slot = Some(syn::parse2(name_value.value.to_token_stream())?);
    }

    for attr in attrs {
        if !attr.path().is_ident("sea_orm") {
            continue;
        }
        let metas: Punctuated<Meta, Token![,]> =
            attr.parse_args_with(Punctuated::parse_terminated)?;
        for meta in metas {
            let path = meta.path().clone();
            if path.is_ident("table_name") || path.is_ident("schema_name") {
                return Err(syn::Error::new_spanned(
                    &meta,
                    "表名与 schema 请改用 `#[aurora_entity(table = \"...\", schema = \"...\")]`：\
                     `#[sea_orm(table_name = ...)]` 生成的是编译期表名，运行时前缀对它无效",
                ));
            }
            if path.is_ident("table_iden") {
                return Err(syn::Error::new_spanned(
                    &meta,
                    "`aurora_entity` 不支持 `table_iden`：它要求编译期表名，无法与运行时前缀共存",
                ));
            }
            if path.is_ident("model_ex") || path.is_ident("active_model_ex") {
                let Meta::NameValue(name_value) = &meta else {
                    return Err(syn::Error::new_spanned(
                        &meta,
                        "`model_ex` / `active_model_ex` 需要写成 `= ModelEx` 形式",
                    ));
                };
                let value: Ident = syn::parse2(name_value.value.to_token_stream())?;
                logical.entity_extra.push((path, quote!(#value)));
            }
        }
    }

    Ok(logical)
}

/// 逻辑表名只允许 ASCII 小写标识符（物理表名会直接进 SQL，必须白名单）。
fn validate_identifier(literal: &LitStr) -> syn::Result<()> {
    let value = literal.value();
    let valid = !value.is_empty()
        && value.len() <= 63
        && value.starts_with(|c: char| c.is_ascii_lowercase())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if valid {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            literal,
            "逻辑表名必须是 ASCII 小写标识符（小写字母开头，仅含小写字母、数字、下划线，且不超过 63 字节）",
        ))
    }
}

fn is_named(path: &Path, name: &str) -> bool {
    path.segments
        .last()
        .is_some_and(|segment| segment.ident == name)
}
