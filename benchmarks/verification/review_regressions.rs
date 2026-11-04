use crate::{cleanup, config::Cleanup, stt};
fn rejected(raw:&str,out:&str) { assert!(cleanup::check(raw,out,&Cleanup::default()).is_err(),"unexpectedly accepted {raw:?} -> {out:?}"); }
fn accepted(raw:&str,out:&str) { assert_eq!(cleanup::check(raw,out,&Cleanup::default()),Ok(()),"unexpectedly rejected {raw:?} -> {out:?}"); }
#[test] fn correction_marker_must_not_erase_unretracted_prohibition() { rejected("Do not delete the logs, actually I need the logs tomorrow","I need the logs tomorrow."); }
#[test] fn english_ten_with_polish_name_is_still_required() { rejected("Send ten files to Łukasz","Send files to Łukasz."); }
#[test] fn capitalized_contraction_expands() { accepted("I'm ready to send the report","I am ready to send the report."); }
#[test] fn capitalized_contraction_can_remain() { accepted("Don't delete the report","Don't delete the report."); }
#[test] fn quote_nesting_must_survive() { rejected("\"He said 'hello' yesterday\"","\"He said hello yesterday.\""); }
#[test] fn quoted_sentence_repeats_are_opaque() { let text="\"We will win the game. We will win the game. We will win the game. We will win the game.\""; assert_eq!(stt::collapse_loops(text),(text.into(),false)); }
#[test] fn similar_spelling_cannot_substitute_unrelated_verb() { rejected("Please repair the server","Please repay the server."); }
#[test] fn first_word_name_cannot_transpose_into_another_name() { rejected("Marina will send the report","Marian will send the report."); }
#[test] fn person_must_survive_grammar_edits() { rejected("I will send the report","He will send the report."); }
#[test] fn lexical_like_is_not_always_filler() { rejected("I like the address","I the address."); }
#[test] fn leading_zero_identifier_is_verbatim() { rejected("Use ID 0012 for this record","Use ID 12 for this record."); }
#[test] fn capitalization_disambiguates_battery_type() { assert_eq!(cleanup::fallback("Use AAA batteries.",&Cleanup::default()),"Use AAA batteries."); }
#[test] fn double_wrapper_must_not_be_inserted() { rejected("\"hello world\"","\"\"Hello world.\"\""); }
#[test] fn internal_quote_annotation_is_literal() { let text="He said \"play [Music] now\".";assert_eq!(stt::filter_hallucinations(text),text); }
#[test] fn digital_silence_only_means_constant_samples() { assert!(stt::review_signal(&[0.0,9e-7,-9e-7,0.0])); }
#[test] fn controls_nested_delimiters_are_preserved() { let text="Use [outer (um uh)] here.";assert_eq!(cleanup::fallback(text,&Cleanup::default()),text);assert_eq!(stt::filter_hallucinations(text),text); }
#[test] fn controls_32_token_loops_respect_paragraphs_and_fences() { let unit=(0..32).map(|i|format!("token{i:02}")).collect::<Vec<_>>().join(" ")+".";let looped=vec![unit.as_str();5].join(" ");assert_eq!(stt::collapse_loops(&looped),(unit.clone(),true)); for text in [format!("```text\n{looped}\n```"),vec![unit.as_str();5].join("\n\n")] {assert_eq!(stt::collapse_loops(&text),(text.clone(),false));} }
#[test] fn controls_silence_and_practically_quiet_speech() {assert!(!stt::review_signal(&[0.0;320]));assert!(!stt::review_signal(&[0.125;320]));assert!(stt::review_signal(&[0.0,1e-4,-1e-4,0.0]));assert!(stt::review_signal(&[0.0,1.0/32768.0,-1.0/32768.0]));}
