//! Regression: European Portuguese continuous narration, `estou a` plus an
//! infinitive, was only partly covered by the phantom detector (#2224).
//!
//! `work_announcement_re` accepted the Brazilian gerund after `estou`
//! ("estou executando") and nothing else, and `plan_announcement_re` accepted
//! `estou a` only when a sequencing tail followed ("... depois"). Plain PT-PT
//! narration, "Estou a carregar os ficheiros.", matched neither arm, so a turn
//! that narrated the work in that shape and called no tool went unflagged.
//!
//! The pt pack now carries both variants in one file: a PT-PT group of
//! `estar a` plus infinitive, which needs no imminence marker because the
//! continuous form cannot describe finished work, and the PT-BR gerund group
//! unchanged, which still needs its marker so a plain gerund sentence stays a
//! statement.

use crate::brain::agent::service::phantom::{
    has_phantom_tool_intent_no_tools, matches_plan_announcement, matches_work_announcement,
};

#[test]
fn bare_pt_pt_continuous_is_a_work_announcement() {
    // The exact shape the issue names: no now / ... / : marker, no tail word.
    assert!(
        matches_work_announcement("Estou a carregar os ficheiros."),
        "bare estou a <infinitivo> must be a work announcement"
    );
    assert!(matches_work_announcement("Estou a executar os testes."));
    assert!(
        has_phantom_tool_intent_no_tools("Estou a carregar os ficheiros"),
        "the zero-tool detector must flag it too"
    );
}

#[test]
fn pt_pt_continuous_covers_the_other_estar_persons() {
    assert!(matches_work_announcement("Estamos a verificar os logs."));
    assert!(matches_work_announcement("Está a compilar o binário."));
    assert!(matches_work_announcement("Estão a analisar o resultado."));
    assert!(matches_work_announcement("Estava a rever o diff."));
    assert!(matches_work_announcement("Estávamos a atualizar o índice."));
}

#[test]
fn pt_pt_state_sentences_stay_out() {
    // "está a rodar bem" describes a state, not a promise of work. The
    // continuous arm refuses a manner adverb directly after the verb so the
    // third person does not turn every status sentence into a phantom.
    assert!(!matches_work_announcement(
        "O servidor está a rodar bem, sem erros."
    ));
    assert!(!matches_work_announcement(
        "A migração estava a correr mal antes do fix."
    ));
}

#[test]
fn pt_br_gerund_still_is_a_work_announcement() {
    // The Brazilian form must keep firing: the fix cannot trade one variant
    // for the other.
    assert!(
        matches_work_announcement("Estou executando os testes agora."),
        "PT-BR gerund with its imminence marker"
    );
    assert!(has_phantom_tool_intent_no_tools(
        "Executando os testes agora."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Estou carregando os arquivos agora."
    ));
}

#[test]
fn pt_br_plain_gerund_sentence_is_still_a_statement() {
    // No marker, no announcement: the gerund arm keeps requiring one.
    assert!(!matches_work_announcement(
        "Executando os testes revelou três falhas."
    ));
}

#[test]
fn sequenced_pt_pt_plan_still_matches_the_plan_arm() {
    // The plan arm keeps its optional `estou a` prefix for the sequenced
    // shape. Both arms may now match this sentence, and that is fine: the
    // detectors OR their arms rather than tallying them, so one sentence
    // yields one flag, not two.
    let sequenced = "Estou a configurar o ambiente, depois verifico os testes.";
    assert!(matches_plan_announcement(sequenced));
    assert!(matches_work_announcement(sequenced));
    assert!(has_phantom_tool_intent_no_tools(sequenced));
}
