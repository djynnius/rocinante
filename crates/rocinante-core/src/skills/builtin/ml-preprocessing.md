---
name: ml-preprocessing
description: "Prepare data for machine learning: identify features and label, train/test split, collinearity heatmap, one-hot and ordinal encoding, binary label to 0/1, standard or min-max scaling, feature reduction with Lasso/PCA/LDA. Use when asked to preprocess data for ML, engineer or encode features, scale variables, split data, or reduce features."
---

# ML Preprocessing

Follow the steps IN ORDER — the order prevents data leakage. Run code with the `bash` tool; keep everything in one script (`write` tool). At every **ASK** step: ask the user and give your recommendation; if you are running as a subagent, stop and return the question + recommendation in your report instead of guessing.

1. **Understand the variables first.** If the dataset has not been explored (types, missingness, codebook), call the `skill` tool with `{"name": "exploratory-data-analysis"}` and do that first. Then agree with the user on:
   - The **label** (outcome) column.
   - The candidate **features**.
   - Exclusions: ID columns, free text, and any variable recorded AFTER the outcome (leakage — a feature that already contains the answer). List the exclusions and why.

2. **Split BEFORE anything is fitted** — the one unbreakable rule:
```python
import pandas as pd, numpy as np, joblib
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from sklearn.model_selection import train_test_split

df = pd.read_csv("FILL_IN.csv")
y = df["LABEL"]
X = df.drop(columns=["LABEL", "ID_COL"])
X_train, X_test, y_train, y_test = train_test_split(
    X, y, test_size=0.2, random_state=42,
    stratify=y if y.nunique() < 20 else None)   # stratify for classification
print(X_train.shape, X_test.shape)
```
   Every scaler, encoder, imputer, and reducer below is fitted on `X_train` only.

3. **Collinearity check with heatmap:**
```python
corr = X_train.corr(numeric_only=True)
plt.figure(figsize=(10, 8))
im = plt.imshow(corr, cmap="coolwarm", vmin=-1, vmax=1)
plt.colorbar(im)
plt.xticks(range(len(corr)), corr.columns, rotation=90)
plt.yticks(range(len(corr)), corr.columns)
plt.tight_layout(); plt.savefig("ml_output/corr_heatmap.png"); plt.close()

pairs = [(a, b, corr.loc[a, b]) for i, a in enumerate(corr.columns)
         for b in corr.columns[i+1:] if abs(corr.loc[a, b]) > 0.8]
print(pairs)   # highly collinear pairs
```
   For each pair with |r| > 0.8: **ASK** which to drop — recommend keeping the one that is more interpretable or preferred by the codebook. (For a formal VIF check, the `statistical-modeling` skill has the snippet.)

4. **Encode categoricals** — pick per column from this table:
   | Column | Encoder |
   |---|---|
   | Binary label (e.g. yes/no) | map to 0/1: `y.map({"no": 0, "yes": 1})` — STATE which class is 1 (the positive class) |
   | Multi-class label | `LabelEncoder` (report the class ↔ integer mapping) |
   | Nominal feature (no order) | `OneHotEncoder(drop="first", handle_unknown="ignore")` |
   | Ordinal feature (real order) | `OrdinalEncoder(categories=[["low","mid","high"]])` — order given explicitly |

   Never one-hot the label. Never ordinal-encode a nominal feature (it invents a fake order).

5. **Scale numerics.** Default `StandardScaler`; **ASK** if `MinMaxScaler` is preferred — recommend MinMax when features must stay in [0,1] (neural nets, bounded inputs), Standard otherwise.
   | Model family | Scaling? |
   |---|---|
   | KNN, SVM/SVR, PCA, Lasso/Ridge, penalized logistic | REQUIRED |
   | Linear/logistic (unpenalized) | recommended |
   | Decision tree, random forest, XGBoost | not needed |

   Enforce fit-on-train-only with the canonical pipeline — this object IS the deliverable:
```python
from sklearn.compose import ColumnTransformer
from sklearn.preprocessing import StandardScaler, OneHotEncoder
from sklearn.pipeline import Pipeline

num_cols = ["FILL", "IN"]          # numeric feature names
cat_cols = ["FILL", "IN"]          # nominal feature names
pre = ColumnTransformer([
    ("num", StandardScaler(), num_cols),
    ("cat", OneHotEncoder(drop="first", handle_unknown="ignore"), cat_cols),
])
pre.fit(X_train)                    # train only — transform test later with .transform
joblib.dump(pre, "ml_output/preprocessor.joblib")
```

6. **Feature reduction — ASK before doing any of it** ("do you want feature reduction? options below"):
   | Method | What it does | Recommend when |
   |---|---|---|
   | Lasso (`LassoCV` / `LogisticRegressionCV(penalty="l1", solver="saga")`) | zeroes out weak features; keeps real, interpretable columns | many correlated features AND interpretability matters (default recommendation) |
   | PCA (`PCA(n_components=0.95)`) | unsupervised compression; components replace features, interpretability lost | pure dimensionality reduction, p very large |
   | LDA (`LinearDiscriminantAnalysis`) | supervised, classification only, at most n_classes−1 components | maximizing class separation |
   Lasso and PCA and LDA all require scaled inputs (step 5). Report which features survived (Lasso: `np.array(features)[model.coef_ != 0]`).

7. **Deliver**: `ml_output/preprocessor.joblib`, the split shapes, the heatmap path, and a decisions log (label + positive class, exclusions, drops from step 3, encoder per column, scaler choice, reduction choice). Next: call the `skill` tool with `{"name": "ml-modeling"}`.

## Rules

- Split first (step 2); every fit uses train only; the test set is transformed, never fitted. Breaking this invalidates all later metrics.
- `random_state=42` on every split/model for reproducibility.
- `mkdir -p ml_output` first; figures saved there, never `plt.show()`.
- Run with `python3` (fallback `python`); install with `python3 -m pip install scikit-learn pandas matplotlib`.
- R projects: tidymodels — `initial_split(df, prop=0.8, strata=label)`, `recipe(label ~ ., train) |> step_corr(all_numeric_predictors(), threshold=0.8) |> step_dummy(all_nominal_predictors()) |> step_normalize(all_numeric_predictors())`, then `prep()`/`bake()`. Same rule: recipe is prepped on training data only.
- Data too big for pandas: pyspark.ml (`VectorAssembler` + `StandardScaler` on a Spark DataFrame) — or aggregate down with the `duckdb` skill first.
- If a step errors, read the message and fix that exact problem (usually a column name or an unencoded string column).
