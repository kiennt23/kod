use kod_llm_proxy::{
    budget::{IMAGE_TOKENS, Ledger, RESERVE_SLACK, Spend, cost, reservation},
    config::{Budget, Price},
};
use serde_json::{Map, Value, json};

const PRICE: Price = Price {
    input: 3,
    output: 15,
    cache_write: 4,
    cache_read: 1,
};

fn usage(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn a_reservation_covers_the_body_at_the_dearest_input_rate_and_max_tokens() {
    let r = reservation(&PRICE, 1000, 0, 50);
    assert_eq!(r.tokens, 1000 + RESERVE_SLACK + 50);
    assert_eq!(r.pico_usd, (1000 + RESERVE_SLACK) * 4 + 50 * 15);
}

#[test]
fn each_image_reserves_its_largest_cost_whatever_its_bytes() {
    let r = reservation(&PRICE, 1000, 2, 50);
    assert_eq!(r.tokens, 1000 + RESERVE_SLACK + 2 * IMAGE_TOKENS + 50);
    assert_eq!(
        r.pico_usd,
        (1000 + RESERVE_SLACK + 2 * IMAGE_TOKENS) * 4 + 50 * 15
    );
}

#[test]
fn cost_prices_each_usage_field() {
    let u = usage(json!({"input_tokens": 10, "output_tokens": 2,
        "cache_creation_input_tokens": 5, "cache_read_input_tokens": 100}));
    assert_eq!(
        cost(&PRICE, &u),
        Some(Spend {
            tokens: 117,
            pico_usd: 30 + 30 + 20 + 100
        })
    );
    // Missing cache fields count as zero; missing input or output is unknown.
    let u = usage(json!({"input_tokens": 10, "output_tokens": 2,
        "cache_creation_input_tokens": null, "cache_read_input_tokens": null}));
    assert_eq!(cost(&PRICE, &u).unwrap().tokens, 12);
    let u = usage(json!({"input_tokens": 10, "output_tokens": null}));
    assert_eq!(cost(&PRICE, &u), None);
}

fn ledger(tokens: u64, micro_usd: u64) -> Ledger {
    Ledger::new(Budget {
        max_tokens: tokens,
        max_cost_micro_usd: micro_usd,
    })
}

fn spend(tokens: u64, pico_usd: u64) -> Spend {
    Spend { tokens, pico_usd }
}

#[test]
fn reservations_never_pass_the_budget_together() {
    let mut l = ledger(100, 1);
    assert!(l.reserve(spend(60, 0)));
    // Two requests in flight can't together pass the budget.
    assert!(!l.reserve(spend(41, 0)));
    assert!(l.reserve(spend(40, 0)));
    assert!(!l.reserve(spend(1, 0)));
    // The cost limit is held in picodollars: 1 micro-USD is 1_000_000.
    let mut l = ledger(100, 1);
    assert!(!l.reserve(spend(1, 1_000_001)));
    assert!(l.reserve(spend(1, 1_000_000)));
}

#[test]
fn settling_charges_the_actual_cost_and_frees_the_rest() {
    let mut l = ledger(100, 1);
    let r = spend(80, 10);
    assert!(l.reserve(r));
    assert!(l.settle(r, Some(spend(30, 5))));
    assert_eq!(l.spent(), spend(30, 5));
    assert!(l.reserve(spend(70, 0)));
}

#[test]
fn unknown_usage_is_charged_the_whole_reservation() {
    let mut l = ledger(100, 1);
    let r = spend(80, 10);
    assert!(l.reserve(r));
    assert!(l.settle(r, None));
    assert_eq!(l.spent(), r);
    assert!(!l.reserve(spend(21, 0)));
}

#[test]
fn a_cost_above_its_reservation_is_charged_and_halts_the_run() {
    let mut l = ledger(100, 1);
    let r = spend(10, 10);
    assert!(l.reserve(r));
    assert!(!l.settle(r, Some(spend(11, 10))));
    assert_eq!(l.spent(), spend(11, 10));
    assert!(l.halted());
    assert!(!l.reserve(spend(0, 0)));
}

#[test]
fn a_released_reservation_is_not_charged() {
    let mut l = ledger(100, 1);
    let r = spend(100, 0);
    assert!(l.reserve(r));
    l.release(r);
    assert_eq!(l.spent(), Spend::default());
    assert!(l.reserve(r));
}

#[test]
fn a_halted_run_reserves_nothing() {
    let mut l = ledger(100, 1);
    l.halt();
    assert!(!l.reserve(spend(0, 0)));
}
