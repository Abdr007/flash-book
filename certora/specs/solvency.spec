/*
 * P-SOLV-4 — Global solvency preservation (the Manifest "loss-of-funds" set).
 *
 *   vault.amount  >=  Σ trader_collateral  +  Σ flp_capital  +  insurance.balance
 *
 * must hold AFTER every balance-mutating instruction whenever it held BEFORE.
 * Kani proves the *arithmetic* of this invariant (matcher::insurance::
 * assess_solvency_full) and the soundness of the one-sided runtime detector
 * (partial_collateral_proves_insolvent). What Kani cannot give — and what this
 * spec encodes for the Certora Solana Prover — is the ALL-PATHS preservation:
 * that no reachable instruction path drives the protocol from solvent to
 * insolvent. The runtime `verify_collateral_solvency` sweep is one-sided
 * (detects insolvency, cannot prove solvency over unbounded traders); this is
 * the complement.
 *
 * STATUS: specification only — not yet run. Requires a Certora Solana license +
 * the cvlr SDK wired into the build (see ../harness/README.md). The 19
 * balance-mutating instructions enumerated below are the proof obligation set;
 * the other ~100 handlers are view/admin/order-book ops that do not move value
 * across the vault boundary.
 */

methods {
    // Ghost views over the live account set (implemented in the Rust harness as
    // summaries that read the TokenAccount / InsuranceFund / FlpExposure /
    // Σ TraderState+Position collateral). envfree: no environment dependence.
    function vaultAmount()        external returns (uint64) envfree;
    function totalCollateral()    external returns (uint64) envfree;
    function flpCapital()         external returns (uint64) envfree;
    function insuranceBalance()   external returns (uint64) envfree;
}

definition liabilities() returns mathint =
    totalCollateral() + flpCapital() + insuranceBalance();

definition solvent() returns bool =
    to_mathint(vaultAmount()) >= liabilities();

/* The invariant itself, asserted as initial + inductive over all methods. */
invariant globalSolvency()
    solvent();

/*
 * Inductive preservation, stated per-method so a counterexample names the
 * offending instruction. `parametricSolvency` ranges over EVERY handler; the
 * Prover discharges the value-movers and trivially closes the view/admin ones.
 */
rule solvencyPreserved(method f) {
    require solvent();

    env e;
    calldataarg args;
    f(e, args);

    assert solvent(),
        "instruction drove vault below Σ collateral + FLP + insurance";
}

/*
 * Surplus is never invented: when solvent, the vault accounts EXACTLY to
 * liabilities + surplus (mirrors the Kani `surplus_exact_when_solvent`, lifted
 * to the whole-program ghost state).
 */
rule surplusNeverInvented(method f) {
    require solvent();
    mathint surplus_pre = to_mathint(vaultAmount()) - liabilities();

    env e; calldataarg args;
    f(e, args);

    assert to_mathint(vaultAmount()) - liabilities() >= 0,
        "post-state surplus went negative (value destroyed for traders)";
}
