---
name: exploratory-data-analysis
description: "Exploratory data analysis (EDA) done rigorously. Use when asked to explore, summarize, or profile a dataset, run univariate or bivariate analysis, check distributions or normality, report missingness, build a Table 1, or pick the right statistical test for a comparison."
---

# Exploratory Data Analysis

Follow the steps in order. Run all code with the `bash` tool. Write the analysis into a script file (e.g. `eda.py`) with the `write` tool so it can be re-run.

1. **Read the documentation first.** Use the `glob` tool to find context files: `*codebook*`, `*dictionary*`, `*protocol*`, `*SAP*`, `README*`, `*.md` near the data. Use the `read` tool on each. These define what every variable means and how it is coded. If none exist, say "no codebook found" in your report and continue.

2. **Set up and summarize the dataset.** `mkdir -p eda_output`, then start the script with exactly this header and summary:

```python
import pandas as pd, numpy as np
import matplotlib
matplotlib.use("Agg")            # terminal has no display — save files, never plt.show()
import matplotlib.pyplot as plt
from scipy import stats

df = pd.read_csv("FILL_IN.csv")  # or pd.read_parquet(...) / pd.read_excel(...)
print("observations:", df.shape[0])
print("variables:", df.shape[1])
print("missingness proportion per variable:")
print(df.isna().mean().sort_values(ascending=False).round(3))
print(df.dtypes)
```

Report observations, variables, and the missingness proportion of every variable. Flag variables with more than 0.20 missing.

3. **Classify every variable.** The dtype is a hint, not the truth. Apply these rules:
   - The codebook says a number is a code (e.g. `male=1, female=2`) → **categorical**. Recode to labels first: `df["sex"] = df["sex"].map({1: "male", 2: "female"})`.
   - Integer column with fewer than 10 distinct values and no unit of measurement → treat as **categorical or ordinal**, not continuous.
   - Ranked levels (Likert, stage, grade) → **ordinal**: use median/[Q1,Q3] and rank-based tests.
   - Many distinct values on a measurement scale (age, BMI, lab values) → **continuous**.
   - Dates and free text: exclude from the statistics and say so.

4. **Univariate analysis.** For each **numeric** variable:
   - Normality rule (deterministic): normal iff Shapiro-Wilk p > 0.05 AND |skewness| < 1. Non-normal otherwise.
   ```python
   x = df["COL"].dropna()
   sample = x.sample(min(len(x), 5000), random_state=1)
   normal = stats.shapiro(sample).pvalue > 0.05 and abs(stats.skew(x)) < 1
   if normal:
       print(f"COL: mean={x.mean():.2f} sd={x.std():.2f}")
   else:
       print(f"COL: median={x.median():.2f} Q1={x.quantile(.25):.2f} Q3={x.quantile(.75):.2f}")
   print(f"COL: min={x.min()} max={x.max()}")   # always report the range
   plt.figure(); x.plot.box(); plt.savefig("eda_output/COL_box.png"); plt.close()
   ```
   - Check min/max against the codebook's valid range; flag impossible values.

   For each **categorical** variable:
   ```python
   counts = df["COL"].value_counts(dropna=False)
   print(pd.DataFrame({"n": counts, "proportion": (counts / len(df)).round(3)}))
   plt.figure(); counts.plot.bar(); plt.tight_layout()
   plt.savefig("eda_output/COL_bar.png"); plt.close()
   ```
   - Flag levels with n < 5 (they constrain the tests in step 5).

5. **Bivariate analysis.** Pick the test from these tables — no other choices.

   **Numeric vs numeric** (scatter plot: `plt.scatter(x, y)` → savefig):
   | Both normal (rule in step 4)? | Test |
   |---|---|
   | Yes | `stats.pearsonr(x, y)` — R: `cor.test(x, y, method="pearson")` |
   | No, or ordinal | `stats.spearmanr(x, y)` — R: `cor.test(..., method="spearman")` |

   Report the coefficient and p-value.

   **Categorical vs categorical**: build `tab = pd.crosstab(df["A"], df["B"])`; print counts and `pd.crosstab(..., normalize="index")` proportions; plot `tab.plot.bar()` (grouped, frequencies) and `tab.div(tab.sum(1), axis=0).plot.bar(stacked=True)` (stacked, proportions), savefig both.
   | Any expected cell count < 5? | Test |
   |---|---|
   | No | `stats.chi2_contingency(tab)` — R: `chisq.test` |
   | Yes | `stats.fisher_exact(tab)` (2×2) — R: `fisher.test` |

   (Expected counts come from `stats.chi2_contingency(tab)[3]`.)

   **Numeric vs categorical**: report center and spread of the numeric at each level (mean±SD if normal, median [Q1,Q3] if not); plot grouped boxplots `df.boxplot(column="Y", by="GROUP")` → savefig.
   | Numeric distribution | 2 groups | 3+ groups |
   |---|---|---|
   | Normal | `stats.ttest_ind(a, b, equal_var=False)` — R: `t.test` | `stats.f_oneway(*groups)` — R: `aov` |
   | Non-normal / ordinal | `stats.mannwhitneyu(a, b)` — R: `wilcox.test` | `stats.kruskal(*groups)` — R: `kruskal.test` |

6. **Write the report** using exactly this skeleton:
   - Data: file, observations, variables, documentation found (or "none").
   - Missingness: table of proportions; variables over 0.20 flagged.
   - Variable inventory: name, class (continuous/ordinal/categorical), source of that decision (codebook or rule).
   - Univariate: one line per variable with the correct summary and the figure path.
   - Bivariate: for each pair analyzed — test used, why (which table row), statistic, p-value, figure path.
   - Data-quality concerns.

## Rules

- Run code with the `bash` tool. Use `python3` (if missing, try `python`). Check a package with `python3 -c "import pandas"`; install with `python3 -m pip install pandas scipy matplotlib`.
- If the project already uses R (`.R` files, `renv.lock`), use R with tidyverse/ggplot2/gtsummary instead; `gtsummary::tbl_summary(df)` produces the Table 1. Save plots with `ggsave("eda_output/name.png")`.
- Never call `plt.show()` or open a viewer. Always save figures to `eda_output/` and report the path.
- If a command fails, read the error message and fix that exact problem; do not switch approaches blindly.
- Large file (over ~500 MB) or out-of-memory: load the `sql-analytics` skill (call the `skill` tool with `{"name": "sql-analytics"}`) and use DuckDB for the summaries.
- If regression or survival modeling is needed next, call the `skill` tool with `{"name": "statistical-modeling"}`.
