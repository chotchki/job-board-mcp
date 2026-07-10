//! Diff-semantics suite. Every expectation here is HAND-DERIVED from SPEC's change
//! semantics and run through the real `record_snapshot` + `diff_board` — never captured
//! from whatever the engine happens to emit today, which would just freeze in current
//! behavior, bugs and all. These are the cases that are easy to get wrong and expensive
//! to get wrong: the change signal this whole project is supposed to be trustworthy about.

use chrono::{DateTime, Utc};
use job_board_mcp::config::BoardConfig;
use job_board_mcp::model::{
    Ats, AtsToken, BoardId, Comp, CompInterval, CompSource, Currency, Equity, ObitKind, Posting,
    ReqId, WorkplaceType, content_hash,
};
use job_board_mcp::store::{BoardDiff, ChangedPosting, Store};

fn day(n: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000 + n * 86_400, 0).unwrap()
}

fn board() -> BoardConfig {
    BoardConfig {
        id: BoardId::new("acme"),
        ats: Ats::Greenhouse,
        token: AtsToken::new("acme"),
        site: None,
        comp_site_only: false,
        updated_at_unreliable: false,
    }
}

/// A posting whose MATERIAL fields (title, locations, workplace_type, comp, description)
/// are all explicit, so a test can move exactly one and nothing else. content_hash is
/// computed the real way, from the material inputs plus the description.
struct Build {
    req: &'static str,
    title: &'static str,
    locations: Vec<String>,
    workplace: WorkplaceType,
    comp: Comp,
    description: &'static str,
    // Non-material — present so we can prove they DON'T move the hash.
    url: String,
    updated_at: Option<DateTime<Utc>>,
    department: Option<String>,
}

impl Build {
    fn new(req: &'static str) -> Self {
        Self {
            req,
            title: "Staff Engineer",
            locations: vec!["Remote US".to_owned()],
            workplace: WorkplaceType::Remote,
            comp: Comp::None,
            description: "the body",
            url: format!("https://acme.example/{req}"),
            updated_at: None,
            department: None,
        }
    }

    fn build(&self) -> Posting {
        Posting {
            ats: Ats::Greenhouse,
            board_id: BoardId::new("acme"),
            req_id: ReqId::new(self.req),
            title: self.title.to_owned(),
            url: self.url.clone(),
            locations: self.locations.clone(),
            workplace_type: self.workplace,
            remote_scope: None,
            comp: self.comp.clone(),
            equity: Equity::None,
            posted_at: None,
            updated_at: self.updated_at,
            updated_at_unreliable: false,
            department: self.department.clone(),
            employment_type: None,
            content_hash: content_hash(
                self.title,
                &self.locations,
                self.workplace,
                &self.comp,
                Equity::None,
                self.description,
            ),
        }
    }
}

fn usd_band() -> Comp {
    Comp::band(
        Currency::new("USD").unwrap(),
        18_000_000,
        24_000_000,
        CompInterval::Year,
        CompSource::Api,
    )
    .unwrap()
}

// ---- the headline invariant ---------------------------------------------------------

#[tokio::test]
async fn bulk_touched_updated_at_produces_zero_changed() {
    // Several boards bump updated_at on EVERY posting during a reindex. If updated_at fed
    // content_hash, that reindex would report the entire board CHANGED — pure noise. It
    // must not: same material fields, different updated_at → nothing changed.
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_board(&board()).await.unwrap();
    let id = BoardId::new("acme");

    let mut a = Build::new("1");
    a.updated_at = Some(day(0));
    store
        .record_snapshot(&id, day(0), &[a.build()])
        .await
        .unwrap();

    let mut b = Build::new("1");
    b.updated_at = Some(day(1)); // the ONLY difference — a bulk touch
    store
        .record_snapshot(&id, day(1), &[b.build()])
        .await
        .unwrap();

    let diff = store.diff_board(&id).await.unwrap();
    assert_eq!(
        diff,
        BoardDiff::default(),
        "a bulk-touched updated_at is not a change"
    );
}

#[tokio::test]
async fn other_non_material_fields_are_not_changes_either() {
    // url, department (and posted_at, employment_type, remote_scope) are carried but not
    // hashed — moving them is not a CHANGED.
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_board(&board()).await.unwrap();
    let id = BoardId::new("acme");

    let a = Build::new("1");
    store
        .record_snapshot(&id, day(0), &[a.build()])
        .await
        .unwrap();

    let mut b = Build::new("1");
    b.url = "https://acme.example/moved".to_owned();
    b.department = Some("Platform".to_owned());
    store
        .record_snapshot(&id, day(1), &[b.build()])
        .await
        .unwrap();

    assert!(store.diff_board(&id).await.unwrap().changed.is_empty());
}

// ---- material fields DO surface, with the right name --------------------------------

async fn changed_fields_after_moving(mutate: impl FnOnce(&mut Build)) -> Vec<String> {
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_board(&board()).await.unwrap();
    let id = BoardId::new("acme");
    store
        .record_snapshot(&id, day(0), &[Build::new("1").build()])
        .await
        .unwrap();

    let mut b = Build::new("1");
    mutate(&mut b);
    store
        .record_snapshot(&id, day(1), &[b.build()])
        .await
        .unwrap();

    let diff = store.diff_board(&id).await.unwrap();
    assert_eq!(diff.changed.len(), 1, "exactly one changed posting");
    assert_eq!(diff.changed[0].req_id, ReqId::new("1"));
    diff.changed[0].changed_fields.clone()
}

#[tokio::test]
async fn each_material_field_is_named_when_it_moves() {
    assert_eq!(
        changed_fields_after_moving(|b| b.title = "Senior Engineer").await,
        vec!["title"]
    );
    assert_eq!(
        changed_fields_after_moving(|b| b.locations = vec!["New York".to_owned()]).await,
        vec!["locations"]
    );
    assert_eq!(
        changed_fields_after_moving(|b| b.workplace = WorkplaceType::Onsite).await,
        vec!["workplace_type"]
    );
    assert_eq!(
        changed_fields_after_moving(|b| b.comp = usd_band()).await,
        vec!["comp"]
    );
    // Description isn't a stored column, so it's named by elimination when the hash moves.
    assert_eq!(
        changed_fields_after_moving(|b| b.description = "a rewritten body").await,
        vec!["description"]
    );
}

#[tokio::test]
async fn an_in_place_down_level_is_a_changed_title() {
    // The motivating example: a req quietly edited from Staff down to Senior. That's real,
    // and worth catching — a CHANGED with "title".
    let fields = changed_fields_after_moving(|b| {
        b.title = "Senior Engineer"; // was "Staff Engineer"
    })
    .await;
    assert_eq!(fields, vec!["title"]);
}

// ---- the failed-fetch invariant -----------------------------------------------------

/// Models the ONE decision the fetch handler makes: record only on success. A failed
/// fetch (Err) writes nothing.
async fn handle_fetch(
    store: &Store,
    id: &BoardId,
    taken: DateTime<Utc>,
    result: Result<Vec<Posting>, ()>,
) {
    if let Ok(postings) = result {
        store.record_snapshot(id, taken, &postings).await.unwrap();
    }
}

#[tokio::test]
async fn a_failed_fetch_never_poisons_the_diff() {
    // A board goes into maintenance mode for a day (the adapter returns Err). Because that
    // day is never recorded, the next real fetch sees the postings as still-alive — NOT a
    // board full of DEAD rows, which is what recording an empty snapshot would have caused.
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_board(&board()).await.unwrap();
    let id = BoardId::new("acme");

    handle_fetch(
        &store,
        &id,
        day(0),
        Ok(vec![Build::new("1").build(), Build::new("2").build()]),
    )
    .await;
    handle_fetch(&store, &id, day(1), Err(())).await; // maintenance mode: not recorded
    handle_fetch(
        &store,
        &id,
        day(2),
        Ok(vec![Build::new("1").build(), Build::new("2").build()]),
    )
    .await;

    let diff = store.diff_board(&id).await.unwrap();
    assert!(
        diff.dead.is_empty(),
        "the failed day must not have marked anything DEAD"
    );
    assert!(diff.new.is_empty());
    assert!(diff.changed.is_empty());
}

#[tokio::test]
async fn an_empty_but_successful_fetch_does_mark_dead() {
    // The contrast that proves the guard is about SUCCESS, not emptiness: a board that
    // really emptied (Ok with no postings) correctly marks the prior postings DEAD.
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_board(&board()).await.unwrap();
    let id = BoardId::new("acme");

    handle_fetch(&store, &id, day(0), Ok(vec![Build::new("1").build()])).await;
    handle_fetch(&store, &id, day(1), Ok(vec![])).await; // genuinely empty, and successful

    assert_eq!(
        store.diff_board(&id).await.unwrap().dead,
        vec![ReqId::new("1")]
    );
}

// ---- obit suppression, in the suite -------------------------------------------------

#[tokio::test]
async fn an_obit_keeps_a_ghost_out_of_new() {
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_board(&board()).await.unwrap();
    let id = BoardId::new("acme");

    store
        .mark_obit(
            &id,
            "ghost",
            ObitKind::Ghost,
            "never on the primary source",
            day(0),
        )
        .await
        .unwrap();
    store
        .record_snapshot(
            &id,
            day(1),
            &[Build::new("ghost").build(), Build::new("real").build()],
        )
        .await
        .unwrap();

    assert_eq!(
        store.diff_board(&id).await.unwrap().new,
        vec![ReqId::new("real")]
    );
}

// ---- the semantic lock --------------------------------------------------------------

#[tokio::test]
async fn the_full_classification_is_locked_at_a_pinned_pair() {
    // A pinned two-snapshot scenario whose ENTIRE classification is asserted, hand-derived
    // from the rules — the regression floor. day0: {A,B,C}. day1: A retitled, B untouched,
    // C dropped, D added. So NEW={D}, CHANGED={A:title}, DEAD={C}; B is silent.
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_board(&board()).await.unwrap();
    let id = BoardId::new("acme");

    store
        .record_snapshot(
            &id,
            day(0),
            &[
                Build::new("A").build(),
                Build::new("B").build(),
                Build::new("C").build(),
            ],
        )
        .await
        .unwrap();

    let mut a2 = Build::new("A");
    a2.title = "Senior Engineer";
    store
        .record_snapshot(
            &id,
            day(1),
            &[a2.build(), Build::new("B").build(), Build::new("D").build()],
        )
        .await
        .unwrap();

    let diff = store.diff_board(&id).await.unwrap();
    assert_eq!(
        diff,
        BoardDiff {
            new: vec![ReqId::new("D")],
            changed: vec![ChangedPosting {
                req_id: ReqId::new("A"),
                changed_fields: vec!["title".to_owned()],
            }],
            dead: vec![ReqId::new("C")],
        }
    );
}
