//! G5 (roadmap item 1.4) — structural enforcement that handlers reach the
//! funding / health math ONLY through their proven wrappers.
//!
//! The proven suite verifies `assess_margin`, `settle_position_funding` /
//! `route_funding`, and `effective_health_mark` / `health_price_with_staleness`.
//! But a proof only covers the code path it names: a refactor that made a handler
//! call a raw primitive DIRECTLY — re-deriving the funding routing or the
//! staleness-gated health price inline — would bypass the proven wrapper and NO
//! proof would fail. This is a source-scanning CI guard (not a proof) that fails
//! the moment a NEW call site to a sensitive primitive appears outside its
//! allowlisted proven wrapper, so the funnel cannot silently regress.
//!
//! The allowlists below are the SANCTIONED callers. Adding a caller is a
//! deliberate act that must update this file — forcing a reviewer to confirm the
//! new caller is itself a proven wrapper (or to route through an existing one).

#[cfg(test)]
mod tests {
    // Sources embedded at compile time (paths relative to this file, i.e. `src/`).
    const LIB_RS: &str = include_str!("lib.rs");
    const RISK_RS: &str = include_str!("matcher/risk.rs");

    /// Extract the function name from a declaration line, else `None`. Strips
    /// visibility / `async` / `unsafe` / `const` / `extern` prefixes.
    fn fn_name(trimmed: &str) -> Option<String> {
        let mut s = trimmed;
        loop {
            let mut stripped = false;
            for p in [
                "pub(crate) ",
                "pub(in crate) ",
                "pub ",
                "async ",
                "const ",
                "unsafe ",
                "extern \"C\" ",
                "extern ",
            ] {
                if let Some(rest) = s.strip_prefix(p) {
                    s = rest;
                    stripped = true;
                    break;
                }
            }
            if !stripped {
                break;
            }
        }
        let rest = s.strip_prefix("fn ")?;
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }

    /// The enclosing `fn` name of every (non-comment) line in `src` that contains
    /// `needle` — attribution walks back to the nearest preceding `fn` decl.
    fn enclosing_fns(src: &str, needle: &str) -> Vec<String> {
        let mut current = String::from("<top-level>");
        let mut hits = Vec::new();
        for line in src.lines() {
            let t = line.trim_start();
            if t.starts_with("//") || t.starts_with('*') {
                continue; // ignore comments/doc-comments
            }
            if let Some(name) = fn_name(t) {
                current = name;
            }
            if line.contains(needle) {
                hits.push(current.clone());
            }
        }
        hits
    }

    /// HEALTH: handlers may reach the health price ONLY through the staleness-gated
    /// selector, and may never fold in the raw worse-of directly.
    #[test]
    fn health_price_reached_only_via_proven_wrappers() {
        // The raw `worse_of_health_price` must not appear in the handler layer at
        // all — it omits the staleness gate, so a handler using it could liquidate
        // off a stale price. It lives behind `health_price_with_staleness`.
        assert_eq!(
            LIB_RS.matches("worse_of_health_price").count(),
            0,
            "worse_of_health_price must not appear in lib.rs handlers — reach it via \
             health_price_with_staleness / effective_health_mark (G5)"
        );

        // `health_price_with_staleness` (the staleness-gated selector) may be
        // reached in the handler layer only from the proven wrapper
        // `effective_health_mark`, plus `liquidate_position_v2`, which currently
        // INLINES an equivalent staleness gate.
        //
        // TRACKED RESIDUAL: collapse `liquidate_position_v2`'s inlined gate into a
        // call to `effective_health_mark` so this allowlist shrinks to the single
        // proven wrapper. Deferred here because it edits a live liquidation money
        // path and so needs a devnet re-verify cycle, not a same-PR change.
        let allow = ["effective_health_mark", "liquidate_position_v2"];
        let hits = enclosing_fns(LIB_RS, "matcher::liquidation::health_price_with_staleness(");
        assert!(
            !hits.is_empty(),
            "no health_price_with_staleness call sites found — needle stale, update this guard"
        );
        for f in hits {
            assert!(
                allow.contains(&f.as_str()),
                "health_price_with_staleness is called from `{f}`, which is NOT an allowlisted \
                 proven wrapper — route health pricing through effective_health_mark (G5)"
            );
        }
    }

    /// FUNDING: handlers may reach the raw `funding_owed` math only through the
    /// proven funding wrappers.
    #[test]
    fn funding_math_reached_only_via_proven_wrappers() {
        // In the handler crate, raw `funding_owed` may be reached only from
        // `settle_position_funding` (which routes the owed amount through the
        // proven `route_funding`).
        let lib_hits = enclosing_fns(LIB_RS, "funding_owed(");
        assert!(
            !lib_hits.is_empty(),
            "no funding_owed call sites found in lib.rs — needle stale, update this guard"
        );
        for f in lib_hits {
            assert_eq!(
                f, "settle_position_funding",
                "funding_owed is called from `{f}` in lib.rs — handlers must reach funding via \
                 settle_position_funding / route_funding (G5)"
            );
        }

        // In the risk module, `funding_owed` may be reached only from the proven
        // `assess_margin` (its equity term).
        for f in enclosing_fns(RISK_RS, "funding_owed(") {
            assert_eq!(
                f, "assess_margin",
                "funding_owed is called from `{f}` in risk.rs — only assess_margin may (G5)"
            );
        }
    }

    /// INTERNAL TRANSFERS (Track A2): the two sub-account transfer handlers move
    /// collateral ONLY through the proven, conservation-checked core
    /// `xmargin::apply_collateral_transfer` — never via inline arithmetic. This is
    /// what makes the model→real bridge a compiler-enforced guarantee, not a
    /// transcription: the `collateral_transfer_conserves_total` Kani proof binds to
    /// the exact code the handlers run.
    #[test]
    fn internal_transfers_mutate_collateral_only_via_proven_core() {
        const TRANSFER_HANDLERS: [&str; 2] = ["transfer_main_to_sub", "transfer_sub_to_main"];

        // (1) Every `apply_collateral_transfer` call site in lib.rs is one of the
        // two sanctioned transfer handlers — nothing else may perform this move.
        let callers = enclosing_fns(LIB_RS, "apply_collateral_transfer(");
        assert!(
            !callers.is_empty(),
            "apply_collateral_transfer call sites not found in lib.rs — needle stale, update guard"
        );
        for f in &callers {
            assert!(
                TRANSFER_HANDLERS.contains(&f.as_str()),
                "apply_collateral_transfer is called from `{f}`, not an allowlisted transfer \
                 handler — internal collateral moves route through the proven core only (A2)"
            );
        }
        // (2) BOTH handlers actually route through it.
        for h in TRANSFER_HANDLERS {
            assert!(
                callers.iter().any(|f| f == h),
                "{h} no longer routes its collateral move through apply_collateral_transfer (A2)"
            );
        }
        // (3) Neither handler re-introduces inline collateral arithmetic — a raw
        // `.checked_sub(`/`.checked_add(` inside a transfer handler would bypass the
        // proven conservation core.
        for needle in [".checked_sub(", ".checked_add("] {
            for f in enclosing_fns(LIB_RS, needle) {
                assert!(
                    !TRANSFER_HANDLERS.contains(&f.as_str()),
                    "`{f}` contains raw `{needle}` — the internal-transfer balance move must go \
                     through apply_collateral_transfer, not inline arithmetic (A2)"
                );
            }
        }
    }
}
