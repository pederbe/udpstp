#![forbid(unsafe_code)]
//! `#[req("ID", ...)]` marks a test as proving the named requirements from
//! `docs/conformance/requirements/`. The attribute leaves the item unchanged;
//! `cargo xtask req` reads the tags from the source and checks them.

use proc_macro::{TokenStream, TokenTree};

#[proc_macro_attribute]
pub fn req(ids: TokenStream, item: TokenStream) -> TokenStream {
    let mut count = 0;
    for token in ids {
        match token {
            TokenTree::Literal(literal) if literal.to_string().starts_with('"') => count += 1,
            TokenTree::Punct(punct) if punct.as_char() == ',' => {}
            other => {
                return error(&format!(
                    "#[req] takes quoted requirement IDs, found `{other}`"
                ));
            }
        }
    }
    if count == 0 {
        return error("#[req] needs at least one quoted requirement ID");
    }
    item
}

fn error(message: &str) -> TokenStream {
    format!("compile_error!({message:?});")
        .parse()
        .expect("valid tokens")
}
