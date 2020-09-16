//use jwt::header::PrecomputedAlgorithmOnlyHeader as Header;
//pub type Auth = jwt::Token<Header, Claims, jwt::Verified>;

// macro_rules! private_deserialize {
//     ($v:vis struct $i:ident $body:tt) => {
//         $v struct $i $body
//
//         mod detail {
//             use super::$i as Impl;
//             #[derive(serde::Deserialize)]
//             #[serde(remote = "Impl")]
//             #[allow(dead_code)]
//             $v struct Def $body
//
//             #[derive(serde::Deserialize)]
//             #[serde(transparent)]
//             $v struct $i(#[serde(with="Def")] pub Impl);
//         }
//     };
// }

#[allow(clippy::manual_non_exhaustive)]
pub struct Auth {
    pub id: u64,
    _priv: (),
}

mod detail {
    #[derive(serde::Deserialize)]
    #[serde(remote = "super::Auth")]
    #[allow(dead_code, clippy::manual_non_exhaustive)]
    pub struct Def {
        pub id: u64,
        #[serde(skip)]
        _priv: (),
    }

    #[derive(serde::Deserialize)]
    #[serde(transparent)]
    pub struct Auth(#[serde(with = "Def")] pub super::Auth);
}

impl<'a> jwt::VerifyWithKey<Auth> for &'a str {
    fn verify_with_key(self, key: &impl jwt::VerifyingAlgorithm) -> Result<Auth, jwt::Error> {
        let c: detail::Auth = self.verify_with_key(key)?;
        Ok(c.0)
    }
}
