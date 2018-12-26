// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This crate can generate types that implement fast finite field arithmetic.
//!
//! Many error correcting codes rely on some form of finite field of the form GF(2^p), where
//! p is relatively small. Similarly some cryptographic algorithms such as AES use finite field
//! arithmetic.
//!
//! While addition and subtraction can be done quickly using just a simple XOR, multiplication is
//! more involved. To speed things up, you can use a precomputed table of logarithms and use the
//! fact that `log(a* b) = log(a) + log(b)`. Expanding back using the same base as the logarithm
//! gives the result for multiplication. The same method can be applied to division.
//!
//! *WARNING:*
//! The types generated by this library are probably not suitable for cryptographic purposes, as
//! multiplication is not guaranteed to be constant time.
//!
//! *WARNING*
//! Currently only small precomputed tables are supported, the compiler may hang on bigger inputs
//! such as for GF(65536)
//! # Examples
//!
//! ```ignore
//! use g2p;
//! g2p::g2p!(GF16, 4, modulus: 0b10011);
//!
//! let one: GF16 = 1.into();
//! let a: GF16 = 5.into();
//! let b: GF16 = 4.into();
//! let c: GF16 = 7.into();
//! assert_eq!(a + c, 2.into());
//! assert_eq!(a - c, 2.into());
//! assert_eq!(a * b, c);
//! assert_eq!(a / c, one / b);
//! assert_eq!(b / b, one);
//! ```

#![recursion_limit = "128"]
extern crate proc_macro;

use proc_macro::TokenStream;

use quote::quote;

use syn::{
    parse::{
        Parse,
        ParseStream,
    },
    Token,
    parse_macro_input,
};

use g2poly::G2Poly;


struct ParsedInput {
    ident: syn::Ident,
    p: syn::LitInt,
    modulus: Option<syn::LitInt>,
    generator: Option<syn::LitInt>,
}

impl Parse for ParsedInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ident = input.parse()?;
        let _sep: Token![,] = input.parse()?;
        let p = input.parse()?;

        let mut modulus = None;
        let mut generator = None;

        loop {
            let sep: Option<Token![,]> = input.parse()?;
            if sep.is_none() || input.is_empty() {
                break;
            }
            let ident: syn::Ident = input.parse()?;
            let ident_name = ident.to_string();
            let _sep: Token![:] = input.parse()?;
            match ident_name.as_str() {
                "modulus" => {
                    if modulus.is_some() {
                        Err(syn::Error::new(ident.span(), "Double declaration of 'modulus'"))?
                    }
                    modulus = Some(input.parse()?);
                }
                "generator" => {
                    if generator.is_some() {
                        Err(syn::Error::new(ident.span(), "Double declaration of 'generator'"))?
                    }
                    generator = Some(input.parse()?)
                }
                _ => {
                    Err(syn::Error::new(ident.span(), "Expected one of 'modulus' or 'generator'"))?
                }
            }
        }

        Ok(ParsedInput {
            ident,
            p,
            modulus,
            generator,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Settings {
    ident: syn::Ident,
    ident_name: String,
    p_val: u64,
    modulus: G2Poly,
    generator: G2Poly,
}

fn find_modulus_poly(p: u64) -> G2Poly {
    assert!(p < 64);

    let start = (1 << p) + 1;
    let end = (1_u64 << (p + 1)).wrapping_sub(1);

    for m in start..=end {
        let p = G2Poly(m);
        if p.is_irreducible() {
            return p;
        }
    }

    unreachable!("There are irreducible polynomial for any degree!")
}

fn find_generator(m: G2Poly) -> G2Poly {
    let max = m.degree().expect("Modulus must have positive degree");

    for g in 1..(2 << max) {
        let g = G2Poly(g);
        if g.is_generator(m) {
            return g;
        }
    }

    unreachable!("There must be a generator element")
}

impl Settings {
    pub fn from_input(input: ParsedInput) -> syn::Result<Self> {
        let ident = input.ident;
        let ident_name = ident.to_string();
        let p_val = input.p.value();
        let modulus = input.modulus
            .map(|m| G2Poly(m.value()))
            .unwrap_or_else(|| find_modulus_poly(p_val));

        if !modulus.is_irreducible() {
            Err(syn::Error::new(syn::export::Span::call_site(), format!("Modulus {} is not irreducible", modulus)))?;
        }

        let generator = input.generator
            .map(|g| G2Poly(g.value()))
            .unwrap_or_else(|| find_generator(modulus));

        if !generator.is_generator(modulus) {
            Err(syn::Error::new(syn::export::Span::call_site(), format!("{} is not a generator", generator)))?;
        }

        Ok(Settings {
            ident,
            ident_name,
            p_val,
            modulus,
            generator,
        })
    }
}

fn generate_log_tables(gen: G2Poly, modulus: G2Poly) -> (Vec<G2Poly>, Vec<usize>) {
    assert!(modulus.is_irreducible());
    assert!(gen.is_generator(modulus));

    let deg = modulus.degree().expect("0 is not irreducible");
    let p_minus_1 = ((1 << deg) - 1) as usize;

    let mut exp_table = Vec::new();
    let mut log_table = vec![0; p_minus_1 + 1];

    let mut cur_pow = G2Poly(1);
    for i in 0..=p_minus_1 {
        exp_table.push(cur_pow);
        log_table[cur_pow.0 as usize] = i;
        cur_pow = (cur_pow * gen) % modulus;
    }
    (exp_table, log_table)
}

/// Generate a newtype of the given name and implement finite field arithmetic on it.
///
/// The generated type have implementations for [`Add`](::core::ops::Add),
/// [`Sub`](::core::ops::Sub), [`Mul`](::core::ops::Mul) and [`Div`](::core::ops::Div).
///
/// There are also implementations for equality, copy and debug. Conversion from and to the base
/// type are implemented via the From trait.
/// Depending on the size of `p` the underlying type is u8 or u16.
///
/// # Example
/// ```ignore
/// g2p!(
///     GF256,                  // Name of the newtype
///     8,                      // The power of 2 specifying the field size 2^8 = 256 in this
///                             // case.
///     modulus: 0b1_0001_1101, // The reduction polynomial to use, each bit is a coffiecient.
///                             // Can be left out in case it is not needed.
///     generator: 0b10         // The element that generates the cyclic group. Can be left out,
///                             // there should not really be a reason to specify it.
/// );
///
/// let a: GF256 = 255.into();  // Conversion from the base type
/// assert_eq!(a - a, a + a);   // Finite field arithmetic.
/// assert_eq!(format("{}", a), "255_GF256");
/// ```
#[proc_macro]
pub fn g2p(input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(input as ParsedInput);
    let settings = Settings::from_input(args).unwrap();
    let ident = settings.ident;
    let ident_name = settings.ident_name;
    let modulus = settings.modulus;
    let generator = settings.generator;
    let p = settings.p_val;
    let field_size = 1_usize << p;
    let mask = (1_u64 << p).wrapping_sub(1);


    let (ty, ari_ty) = match p {
        0 => panic!("p must be > 0"),
        1..=8 => (quote!(u8), quote!(u16)),
        9..=16 => (quote!(u16), quote!(u32)),
        _ => unimplemented!("p > 16 is not implemented right now"),
    };


    let (exp, log) = generate_log_tables(generator, modulus);
    let exp = exp.into_iter()
        .map(|p| {
            let v = p.0;
            quote!(#v as #ty)
        });
    let log = log.into_iter()
        .map(|l| {
            quote!(#l as #ty)
        });


    let struct_def = quote! {
        struct #ident(#ty);
    };

    let struct_impl = quote! {
    impl #ident {
        pub const MASK: #ty = #mask as #ty;
        pub const EXP_TABLE: [#ty; #field_size] = [#(#exp,)*];
        pub const LOG_TABLE: [#ty; #field_size] = [#(#log,)*];
    }
    };

    let from = quote![
        impl ::core::convert::From<#ident> for #ty {
            fn from(v: #ident) -> #ty {
                v.0
            }
        }
    ];

    let into = quote![
        impl ::core::convert::From<#ty> for #ident {
            fn from(v: #ty) -> #ident {
                #ident(v & #ident::MASK)
            }
        }
    ];

    let eq = quote![
        impl ::core::cmp::PartialEq<#ident> for #ident {
            fn eq(&self, other: &#ident) -> bool {
                self.0 == other.0
            }
        }

        impl ::core::cmp::Eq for #ident {}
    ];

    let tmpl = format!("{{}}_{}", ident_name);
    let debug = quote![
        impl ::core::fmt::Debug for #ident {
            fn fmt<'a>(&self, f: &mut ::core::fmt::Formatter<'a>) -> ::core::fmt::Result {
                write!(f, #tmpl, self.0)
            }
        }
    ];
    let display = quote![
        impl ::core::fmt::Display for #ident {
            fn fmt<'a>(&self, f: &mut ::core::fmt::Formatter<'a>) -> ::core::fmt::Result {
                write!(f, #tmpl, self.0)
            }
        }
    ];
    let clone = quote![
        impl ::core::clone::Clone for #ident {
            fn clone(&self) -> Self {
                *self
            }
        }
    ];
    let copy = quote![
        impl ::core::marker::Copy for #ident {}
    ];
    let add = quote![
        impl ::core::ops::Add for #ident {
            type Output = #ident;

            fn add(self, rhs: #ident) -> #ident {
                #ident(self.0 ^ rhs.0)
            }
        }
        impl ::core::ops::AddAssign for #ident {
            fn add_assign(&mut self, rhs: #ident) {
                *self = *self + rhs;
            }
        }
    ];
    let sub = quote![
        impl ::core::ops::Sub for #ident {
            type Output = #ident;
            fn sub(self, rhs: #ident) -> #ident {
                #ident(self.0 ^ rhs.0)
            }
        }
        impl ::core::ops::SubAssign for #ident {
            fn sub_assign(&mut self, rhs: #ident) {
                *self = *self - rhs;
            }
        }
    ];
    let mul = quote![
        impl ::core::ops::Mul for #ident {
            type Output = #ident;
            fn mul(self, rhs: #ident) -> #ident {
                if self.0 == 0 || rhs.0 == 0 {
                    return #ident(0);
                }

                let a = #ident::LOG_TABLE[self.0 as usize] as #ari_ty;
                let b = #ident::LOG_TABLE[rhs.0 as usize] as #ari_ty;

                let mut c = a + b;
                if c > (#field_size as #ari_ty - 1) {
                    c -= #field_size as #ari_ty - 1;
                }
                #ident(#ident::EXP_TABLE[c as usize])
            }
        }
        impl ::core::ops::MulAssign for #ident {
            fn mul_assign(&mut self, rhs: #ident) {
                *self = *self * rhs;
            }
        }
    ];

    let err_msg = format!("Division by 0 in {}", ident_name);
    let div = quote![
        impl ::core::ops::Div for #ident {
            type Output = #ident;

            fn div(self, rhs: #ident) -> #ident {
                if rhs.0 == 0 {
                    panic!(#err_msg);
                }

                let a = #ident::LOG_TABLE[self.0 as usize] as #ari_ty;
                let inv_rhs = #ident::LOG_TABLE[rhs.0 as usize] as #ari_ty;
                let mut c = #field_size as #ari_ty - 1 + a - inv_rhs;
                if c > (#field_size as #ari_ty - 1) {
                    c -= #field_size as #ari_ty - 1;
                }
                #ident(#ident::EXP_TABLE[c as usize])
            }
        }
        impl ::core::ops::DivAssign for #ident {
            fn div_assign(&mut self, rhs: #ident) {
                *self = *self / rhs;
            }
        }
    ];

    TokenStream::from(quote! {
        #struct_def
        #struct_impl
        #from
        #into
        #eq
        #debug
        #display
        #clone
        #copy
        #add
        #sub
        #mul
        #div
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_parser() {
        let span = syn::export::Span::call_site();

        let input = ParsedInput {
            ident: syn::Ident::new("foo", span),
            p: syn::LitInt::new(3, syn::IntSuffix::None, span),
            modulus: None,
            generator: None,
        };

        let r = Settings::from_input(input);
        assert!(r.is_ok());
        assert_eq!(r.unwrap(), Settings {
            ident: syn::Ident::new("foo", span),
            ident_name: "foo".to_string(),
            p_val: 3,
            modulus: G2Poly(0b1011),
            generator: G2Poly(0b10),
        });
    }

    #[test]
    fn test_generate_log_table() {
        let m = G2Poly(0b100011101);
        let g = G2Poly(0b10);

        let (exp, log) = generate_log_tables(g, m);
        assert_eq!(exp.len(), log.len());

        for (i, l) in log.iter().enumerate().skip(1) {
            assert_eq!(G2Poly(i as u64), exp[*l]);
        }
    }

    #[test]
    #[should_panic]
    fn test_generate_log_should_fail() {
        let m = G2Poly(0b100011011);
        let g = G2Poly(0b10);

        generate_log_tables(g, m);
    }
}
