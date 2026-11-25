//! Reading and writing settings; secrets are write-only.
//!
//! Operations land here as their GUI doors move behind the contract.

use serde::{Deserialize, Serialize};

use crate::db::repository::HintDismissalRepository;
use crate::ui_core::hints::DISMISS_DAYS;

use super::{BackendError, Ctx, Op, Scope};

/// The hint ids hidden right now (#3120, #3121).
///
/// The hint engine never reads a database, so the window asks for the set
/// and hands it over as plain state. A failure is carried as its `{e:#}`
/// text; the window reads any failure as "nothing hidden", so a broken read
/// leaves the hints showing rather than blanking them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HintDismissals {}

impl Op for HintDismissals {
    const NAME: &'static str = "settings.hint_dismissals";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<String>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = HintDismissalRepository::new(db.pool().clone());
        repo.active()
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// Hide one hint for [`DISMISS_DAYS`].
///
/// The reply is the write's failure, worded for the log, or `None` when it
/// landed. A ledger that cannot be opened is the `Err`: the window has always
/// let that one pass in silence, since the line coming back on the next
/// launch is the safe direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissHint {
    /// The hint's id, as the engine names it.
    pub hint_id: String,
}

impl Op for DismissHint {
    const NAME: &'static str = "settings.dismiss_hint";
    const SCOPE: Scope = Scope::Send;
    type Reply = Option<String>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = HintDismissalRepository::new(db.pool().clone());
        Ok(repo
            .dismiss(&self.hint_id, DISMISS_DAYS)
            .await
            .err()
            .map(|e| format!("hint dismissal write: {e}")))
    }
}

ops! { HintDismissals, DismissHint }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    #[tokio::test]
    async fn a_dismissed_hint_reads_back_as_hidden() {
        let (_dir, ctx) = crate::backend::ctx::temp_ctx().await;
        assert_eq!(
            HintDismissals {}.run(&ctx).await.expect("read"),
            Vec::<String>::new()
        );
        let written = DismissHint {
            hint_id: "clear".into(),
        }
        .run(&ctx)
        .await
        .expect("the ledger opens");
        assert_eq!(written, None, "the write landed");
        assert_eq!(
            HintDismissals {}.run(&ctx).await.expect("read"),
            vec!["clear".to_string()]
        );
    }

    #[tokio::test]
    async fn hint_dismissals_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            DismissHint {
                hint_id: "clear".into(),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": null }));
        let answer = parity!(gateway, HintDismissals {});
        assert_eq!(answer, serde_json::json!({ "Ok": ["clear"] }));

        forbidden!(gateway, HintDismissals {});
        forbidden!(
            gateway,
            DismissHint {
                hint_id: "refused".into(),
            }
        );
        let answer = parity!(gateway, HintDismissals {});
        assert_eq!(
            answer,
            serde_json::json!({ "Ok": ["clear"] }),
            "a refused dismissal wrote nothing"
        );
    }
}
