---
name: statistical-modeling
description: "Regression and survival modeling with correct diagnostics and interpretation. Use when asked to fit or interpret a linear/multiple regression, logistic regression, check collinearity or VIF, model a binary or time-to-event outcome, or run survival analysis (Kaplan-Meier, Cox, Weibull)."
---

# Statistical Modeling

Follow the steps in order. Run all code with the `bash` tool; keep it in a script file. If the variables have not been characterized yet, first call the `skill` tool with `{"name": "exploratory-data-analysis"}` and do that — model choices depend on it.

1. **Choose the model from the outcome type.** No other criteria.

   | Outcome | Model | Go to |
   |---|---|---|
   | Continuous | Multiple linear regression | step 3 |
   | Binary (yes/no) | Logistic regression | step 4 |
   | Time-to-event with censoring | Survival analysis | step 5 |
   | Count | Poisson (negative binomial if overdispersed) | fit like step 3 with `smf.poisson` |

   Choose covariates from the protocol/SAP or subject-matter reasoning. Do not use stepwise selection by p-value.

2. **Check collinearity with VIF before interpreting anything.**

```python
import pandas as pd, numpy as np
import statsmodels.api as sm
from statsmodels.stats.outliers_influence import variance_inflation_factor

X = sm.add_constant(df[["FILL", "IN", "PREDICTORS"]].dropna())
for i, col in enumerate(X.columns):
    if col != "const":
        print(col, round(variance_inflation_factor(X.values, i), 1))
```
   R: `car::vif(lm(y ~ x1 + x2, data=df))`.
   Rule: VIF > 5 → report it as a concern. VIF > 10 → drop one of the correlated predictors (keep the more interpretable or protocol-specified one) and refit. Always report the VIFs.

3. **Multiple linear regression** (continuous outcome).

```python
import statsmodels.formula.api as smf
res = smf.ols("y ~ x1 + x2 + group", data=df).fit()
print(res.summary())          # coefficients, 95% CI, adjusted R²
```
   Check assumptions on the fitted model, each with a saved plot (`matplotlib.use("Agg")`, savefig — never `plt.show()`):
   - Residuals vs fitted (`plt.scatter(res.fittedvalues, res.resid)`) — no curve, no funnel.
   - QQ plot of residuals: `sm.qqplot(res.resid, line="45")`.
   - If residuals funnel (heteroscedasticity): refit with `.fit(cov_type="HC3")`. If curved: log-transform the outcome and refit.
   Interpretation template: "A 1-unit increase in X is associated with a β change in Y (95% CI L to U), holding the other covariates fixed." With a log outcome: "a (exp(β)−1)×100% change".

4. **Logistic regression** (binary outcome).

```python
res = smf.logit("event ~ x1 + x2 + group", data=df).fit()
or_table = pd.DataFrame({"OR": np.exp(res.params),
                         "CI_low": np.exp(res.conf_int()[0]),
                         "CI_high": np.exp(res.conf_int()[1]),
                         "p": res.pvalues}).round(3)
print(or_table)
```
   R: `glm(event ~ x1 + x2, family=binomial, data=df)`; ORs: `exp(cbind(coef(m), confint(m)))`.
   - Rule of thumb: at least 10 events per predictor; fewer → drop predictors and say so.
   - Huge standard errors or no convergence = separation → use Firth logistic (R `logistf`) or remove the offending predictor.
   Interpretation template: "OR 1.8 (95% CI 1.2–2.6): 80% higher odds of EVENT per unit of X (or vs the reference level), adjusted for the other covariates." Never call an odds ratio a risk ratio.

5. **Survival analysis** (time-to-event). Define time origin, event, and censoring explicitly first. Python `lifelines` (`python3 -m pip install lifelines`) or R `survival` + `survminer`.
   1. Descriptive first — Kaplan-Meier / life table per group, median survival, log-rank test:
   ```python
   from lifelines import KaplanMeierFitter
   from lifelines.statistics import logrank_test
   kmf = KaplanMeierFitter()
   ax = None
   for name, g in df.groupby("group"):
       kmf.fit(g["time"], g["event"], label=str(name))
       ax = kmf.plot_survival_function(ax=ax)
       print(name, "median survival:", kmf.median_survival_time_)
   plt.savefig("model_output/km.png"); plt.close()
   ```
   R: `survfit(Surv(time, event) ~ group)`, `survdiff` for log-rank, `survminer::ggsurvplot(..., risk.table=TRUE)`.
   2. Cox proportional hazards for covariate adjustment:
   ```python
   from lifelines import CoxPHFitter
   cph = CoxPHFitter().fit(df[["time","event","x1","x2"]], "time", "event")
   cph.print_summary()                    # HRs with 95% CI
   print(cph.check_assumptions(df[["time","event","x1","x2"]]))  # PH test
   ```
   R: `coxph(...)`, PH check with `cox.zph(m)`.
   3. Rule: if the PH check fails for a covariate (p < 0.05), either stratify on it (`strata(x)` in R, `strata=` in lifelines) or fit a parametric Weibull model (`lifelines.WeibullAFTFitter` / R `survreg(dist="weibull")`) and compare AIC.
   Interpretation template: "HR 1.5 (95% CI 1.1–2.1): 50% higher hazard of EVENT vs the reference, adjusted for the other covariates." Report the number of events and follow-up time.

6. **Report**, in this order: model and why (outcome type); N used and how missing data were handled; VIF table; assumption checks performed and their outcomes; the coefficient table (estimate, 95% CI, p) — β, OR, or HR with a template sentence for each key predictor; figure paths. Label anything not pre-specified as exploratory.

## Rules

- Run code with the `bash` tool. Use `python3` (if missing, try `python`). Install packages with `python3 -m pip install statsmodels lifelines`. In R projects use R (`Rscript`).
- Never call `plt.show()`. `mkdir -p model_output` first; save every figure there and report the path.
- Report effect sizes with 95% CIs, never a bare p-value.
- If a command fails, read the error message and fix that exact problem before trying anything else.
