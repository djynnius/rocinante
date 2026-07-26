---
name: ml-modeling
description: "Choose, train, and tune supervised ML models: regression (linear, polynomial, tree, random forest, SVR, XGBoost) and classification (logistic, KNN, tree, random forest, SVC, XGBoost), cross-validation, nested CV, hyperparameter tuning with Optuna or sklearn. Use when asked to train an ML model, pick a model, tune hyperparameters, or set up cross-validation."
---

# ML Modeling

Prerequisite: preprocessed data with a fitted preprocessor (call the `skill` tool with `{"name": "ml-preprocessing"}` if not done). Run code with the `bash` tool. At every **ASK** step: ask the user with your recommendation; as a subagent, stop and return the question + recommendation in your report.

1. **Route by the label, and fit baselines FIRST.** Numeric label → regression. Categorical label → classification. Every model must beat two baselines or it is worthless:
```python
from sklearn.dummy import DummyRegressor, DummyClassifier
from sklearn.linear_model import LinearRegression, LogisticRegression
# regression: DummyRegressor(strategy="mean") then LinearRegression()
# classification: DummyClassifier(strategy="most_frequent") then LogisticRegression(max_iter=1000)
```

2. **Pick the model from the menu — ASK, with a recommendation from the table.** Strong default for tabular data: RandomForest or XGBoost. When interpretability is the point: linear/logistic.

   **Regression** (numeric label):
   | Model | sklearn | Recommend when |
   |---|---|---|
   | Multiple linear | `LinearRegression()` | relationships look linear; interpretability |
   | Polynomial | `Pipeline([("poly", PolynomialFeatures(degree=2)), ("lr", LinearRegression())])` | visible curvature; keep degree ≤ 3 |
   | Decision tree | `DecisionTreeRegressor(max_depth=5, random_state=42)` | need explainable rules |
   | Random forest | `RandomForestRegressor(n_estimators=300, random_state=42)` | strong tabular default |
   | SVR | `SVR(C=1.0)` — scaled features required | n < ~10k, nonlinear |
   | XGBoost | `XGBRegressor(n_estimators=300, learning_rate=0.05, random_state=42)` | best accuracy on tabular; `python3 -m pip install xgboost` |

   **Classification** (categorical label):
   | Model | sklearn | Recommend when |
   |---|---|---|
   | Logistic | `LogisticRegression(max_iter=1000)` | baseline + interpretable odds ratios |
   | KNN | `KNeighborsClassifier(n_neighbors=5)` — scaled required | small n, local structure |
   | Decision tree | `DecisionTreeClassifier(max_depth=5, random_state=42)` | explainable rules |
   | Random forest | `RandomForestClassifier(n_estimators=300, class_weight="balanced", random_state=42)` | strong tabular default |
   | SVC | `SVC(C=1.0, probability=True)` — scaled required | n < ~10k, nonlinear boundary |
   | XGBoost | `XGBClassifier(n_estimators=300, learning_rate=0.05, eval_metric="logloss", random_state=42)` | best accuracy on tabular |

   Always train as ONE pipeline so preprocessing travels with the model:
```python
from sklearn.pipeline import Pipeline
model = Pipeline([("pre", pre), ("clf", RandomForestClassifier(n_estimators=300, random_state=42))])
model.fit(X_train, y_train)
```

3. **Validation — pick from this table:**
   | Need | Method |
   |---|---|
   | Quick estimate | the held-out test set (once, at the very end — see Rules) |
   | Robust estimate | 5-fold CV: `cross_val_score(model, X_train, y_train, cv=KFold(5, shuffle=True, random_state=42))` — classification uses `StratifiedKFold` |
   | Tune AND report honest performance | **nested CV**: |
```python
from sklearn.model_selection import GridSearchCV, cross_val_score, StratifiedKFold
inner = GridSearchCV(model, param_grid, cv=StratifiedKFold(3, shuffle=True, random_state=42))
scores = cross_val_score(inner, X_train, y_train, cv=StratifiedKFold(5, shuffle=True, random_state=42))
print(scores.mean(), scores.std())   # honest estimate; then inner.fit(...) for the final model
```

4. **Hyperparameter tuning.** Start cheap, escalate:
   - First: `RandomizedSearchCV(model, param_distributions, n_iter=30, cv=5, random_state=42)`.
   - Recommended for efficiency: **Optuna** (`python3 -m pip install optuna`) — smarter search + pruning:
```python
import optuna
from sklearn.model_selection import cross_val_score

def objective(trial):
    clf = RandomForestClassifier(
        n_estimators=trial.suggest_int("n_estimators", 100, 600),
        max_depth=trial.suggest_int("max_depth", 3, 20),
        min_samples_leaf=trial.suggest_int("min_samples_leaf", 1, 10),
        random_state=42)
    pipe = Pipeline([("pre", pre), ("clf", clf)])
    return cross_val_score(pipe, X_train, y_train, cv=5).mean()

study = optuna.create_study(direction="maximize")
study.optimize(objective, n_trials=50, show_progress_bar=False)
print(study.best_params, study.best_value)
```
   - Ray Tune only when tuning must be distributed across machines/GPUs — mention it, do not default to it.
   Key search spaces: RF → n_estimators, max_depth, min_samples_leaf; XGB → learning_rate (0.01–0.3 log), max_depth (3–10), n_estimators (+ `early_stopping_rounds=20` with an eval set); SVM → C (log 1e-2..1e2), gamma (log 1e-4..1).

5. **Overfit and imbalance checks:**
   - Train score much higher than CV score (gap > ~0.05–0.1) → overfitting: reduce depth, add regularization, or get more data. Report both numbers.
   - Imbalanced classes: keep `stratify=`/`StratifiedKFold`; set `class_weight="balanced"` (sklearn) or `scale_pos_weight=neg/pos` (XGBoost). Decision-threshold tuning happens in evaluation, not here.

6. **Save and hand off**: `joblib.dump(final_model, "ml_output/model.joblib")` (the FULL pipeline — preprocessing + model in one object, so predictions on new data cannot skip the transforms). Then call the `skill` tool with `{"name": "ml-evaluation"}`.

## Rules

- The test set is touched EXACTLY ONCE — final evaluation, after all model and hyperparameter choices are frozen. Tuning uses CV on the training set only.
- Always tune the Pipeline (preprocessor inside the CV loop), never pre-transformed data — otherwise CV leaks.
- `random_state=42` everywhere; report CV mean ± std, not a single fold.
- Run with `python3`; install with `python3 -m pip install scikit-learn xgboost optuna`.
- R projects: tidymodels — `parsnip` spec (`rand_forest(trees=300) |> set_engine("ranger") |> set_mode("classification")`), `workflow() |> add_recipe(rec) |> add_model(spec)`, tune with `tune_grid(resamples=vfold_cv(train, v=5), grid=20)`, finalize with `select_best()`.
- Data too large for one machine: pyspark.ml (`RandomForestClassifier`, `CrossValidator`) — say so instead of letting pandas OOM.
- If training errors, read the message: almost always an unencoded string column or NaNs → fix in preprocessing, not by deleting rows silently.
