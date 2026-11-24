//! The backend contract: every thing a surface asks of a crab, as a named,
//! serializable operation.
//!
//! A surface never opens the ledger or holds the agent service itself. It
//! builds an [`Op`] and hands it to a backend. Locally that runs the op
//! against the primary [`Ctx`]; a remote backend sends the same value over the
//! wire and the other crab runs it through [`dispatch`]. Because both sides go
//! through the same `Op::run`, a local answer and a remote answer can only
//! differ by the transport.
//!
//! Each domain file declares its operations with `ops!{}`. That macro writes
//! the domain's own dispatch and registry, so adding an operation touches one
//! file only. This file lists domains and nothing else.

/// Declare a domain's operations.
///
/// Generates, in the file that calls it, `handles(name)`, `dispatch(..)` and
/// `registry()` for the listed types. Every listed type must implement
/// [`Op`]. An empty list is valid: it is how a domain exists before its first
/// operation lands.
macro_rules! ops {
    ($($op:ty),* $(,)?) => {
        /// Whether one of this domain's operations is called `name`.
        #[allow(dead_code)]
        pub(crate) fn handles(name: &str) -> bool {
            registry().iter().any(|(known, _)| *known == name)
        }

        /// Run the operation called `name` with JSON `params`.
        ///
        /// The caller checks [`handles`] and the scope first; an unknown name
        /// here is still answered rather than assumed impossible.
        #[allow(dead_code)]
        pub(crate) async fn dispatch(
            ctx: &$crate::backend::Ctx,
            name: &str,
            params: ::serde_json::Value,
        ) -> ::std::result::Result<::serde_json::Value, $crate::backend::BackendError> {
            $(
                if name == <$op as $crate::backend::Op>::NAME {
                    return $crate::backend::op::run_json::<$op>(ctx, params).await;
                }
            )*
            let _ = (ctx, params);
            Err($crate::backend::BackendError::UnknownOp(name.to_string()))
        }

        /// Every operation this domain declares, with the scope it needs.
        pub(crate) fn registry() -> ::std::vec::Vec<(&'static str, $crate::backend::Scope)> {
            ::std::vec![
                $( (<$op as $crate::backend::Op>::NAME, <$op as $crate::backend::Op>::SCOPE) ),*
            ]
        }
    };
}

/// Declare the domains, and from them the crate-wide dispatch and registry.
macro_rules! domains {
    ($($domain:ident),* $(,)?) => {
        $( pub mod $domain; )*

        /// Run the operation called `name`, with every scope granted.
        ///
        /// This is the local door: the process asking is the crab itself. A
        /// caller that speaks for someone else uses [`dispatch_as`].
        pub async fn dispatch(
            ctx: &Ctx,
            name: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, BackendError> {
            dispatch_as(ctx, Scope::ALL, name, params).await
        }

        /// Run the operation called `name` for a caller holding `granted`.
        ///
        /// The scope check happens before the params are read, so a caller
        /// without the scope learns nothing about what the params should be.
        pub async fn dispatch_as(
            ctx: &Ctx,
            granted: &[Scope],
            name: &str,
            params: serde_json::Value,
        ) -> Result<serde_json::Value, BackendError> {
            let Some(scope) = scope_of(name) else {
                return Err(BackendError::UnknownOp(name.to_string()));
            };
            if !granted.contains(&scope) {
                return Err(BackendError::Forbidden { scope });
            }
            $(
                if $domain::handles(name) {
                    return $domain::dispatch(ctx, name, params).await;
                }
            )*
            Err(BackendError::UnknownOp(name.to_string()))
        }

        /// Every operation of every domain, with the scope it needs.
        pub fn registry() -> Vec<(&'static str, Scope)> {
            let mut all = Vec::new();
            $( all.extend($domain::registry()); )*
            all
        }
    };
}

pub mod cap;
pub mod ctx;
pub mod error;
pub mod event;
pub mod op;
#[cfg(test)]
mod parity;

domains! {
    sessions,
    transcript,
    session_ops,
    turn,
    projects,
    mission,
    usage,
    prompt_files,
    telegram_topics,
    facts,
    tasks,
    fs_list,
    status,
    hello,
    blob,
    voice,
    settings,
    models,
    shell,
}

pub use blob::BlobRef;
pub use cap::Cap;
pub use ctx::{Ctx, primary, set_primary};
pub use error::BackendError;
pub use event::Event;
pub use facts::{Facts, facts_for};
pub use op::{Op, Scope};

/// The scope the operation called `name` needs, or `None` when no domain
/// declares it.
pub fn scope_of(name: &str) -> Option<Scope> {
    registry()
        .into_iter()
        .find(|(known, _)| *known == name)
        .map(|(_, scope)| scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_name_is_declared_once() {
        let names: Vec<&str> = registry().into_iter().map(|(name, _)| name).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            names.len(),
            unique.len(),
            "two operations share a name: {names:?}"
        );
    }

    #[tokio::test]
    async fn an_unknown_operation_is_refused_by_name() {
        let ctx = Ctx::new(std::path::PathBuf::from("unused.db"));
        let err = dispatch(&ctx, "no.such.op", serde_json::Value::Null)
            .await
            .expect_err("an unknown name has no operation to run");
        assert_eq!(err, BackendError::UnknownOp("no.such.op".to_string()));
    }
}
