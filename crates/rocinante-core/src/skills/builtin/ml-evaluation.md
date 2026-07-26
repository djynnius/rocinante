---
name: ml-evaluation
description: "Evaluate ML models properly: RMSE/MAE/MAPE, confusion matrix, precision/recall/F1 with sensitivity/specificity/PPV/NPV, ROC and PR AUC, calibration slope/intercept/Brier score, Platt scaling and isotonic recalibration, feature importance and SHAP. Use when asked to evaluate, compare, calibrate, or explain a trained model or report its metrics."
---

# ML Evaluation

Evaluate on the untouched test set — once. Run code with the `bash` tool; figures to `ml_output/` via Agg + savefig. At every **ASK** step: ask the user with a recommendation; as a subagent, stop and return the question + recommendation in your report.

1. **Regression metrics** (numeric label) — report all four + a residual plot:
```python
from sklearn.metrics import (root_mean_squared_error, mean_absolute_error,
                             mean_absolute_percentage_error, r2_score)
pred = model.predict(X_test)
print("RMSE:", root_mean_squared_error(y_test, pred))   # same units as y
print("MAE :", mean_absolute_error(y_test, pred))
print("MAPE:", mean_absolute_percentage_error(y_test, pred))  # unreliable if y ≈ 0 — say so
print("R²  :", r2_score(y_test, pred))
plt.scatter(pred, y_test - pred, s=8); plt.axhline(0, color="red")
plt.xlabel("predicted"); plt.ylabel("residual")
plt.savefig("ml_output/residuals.png"); plt.close()
```

2. **Classification metrics** — confusion matrix + the full clinical mapping:
```python
from sklearn.metrics import (confusion_matrix, ConfusionMatrixDisplay,
                             classification_report, roc_auc_score)
pred  = model.predict(X_test)
proba = model.predict_proba(X_test)[:, 1]        # positive-class probability
tn, fp, fn, tp = confusion_matrix(y_test, pred).ravel()
print(classification_report(y_test, pred))
print("sensitivity (recall+)      :", tp / (tp + fn))
print("specificity (recall-)      :", tn / (tn + fp))
print("PPV (precision+)           :", tp / (tp + fp))
print("NPV (precision-)           :", tn / (tn + fn))
print("ROC-AUC:", roc_auc_score(y_test, proba))
ConfusionMatrixDisplay.from_predictions(y_test, pred)
plt.savefig("ml_output/confusion.png"); plt.close()
```
   | Clinical term | = sklearn term | Formula |
   |---|---|---|
   | Sensitivity | recall of positive class | TP/(TP+FN) |
   | Specificity | recall of negative class | TN/(TN+FP) |
   | PPV | precision of positive class | TP/(TP+FP) |
   | NPV | precision of negative class | TN/(TN+FN) |

   Imbalanced data: also report PR-AUC (`average_precision_score`) — ROC-AUC alone flatters imbalanced models.

3. **Calibration** (any model that outputs probabilities) — plot, slope/intercept, Brier:
```python
import numpy as np, statsmodels.api as sm
from sklearn.calibration import calibration_curve
from sklearn.metrics import brier_score_loss

frac_pos, mean_pred = calibration_curve(y_test, proba, n_bins=10)
plt.plot(mean_pred, frac_pos, "o-"); plt.plot([0, 1], [0, 1], "--")
plt.xlabel("predicted probability"); plt.ylabel("observed fraction")
plt.savefig("ml_output/calibration.png"); plt.close()

# Calibration slope & intercept: logistic regression of outcome on log-odds
eps = 1e-10
logit = np.log(np.clip(proba, eps, 1 - eps) / np.clip(1 - proba, eps, 1 - eps))
slope = sm.Logit(y_test, sm.add_constant(logit)).fit(disp=0).params
print("calibration intercept:", slope[0], " slope:", slope[1])

brier = brier_score_loss(y_test, proba)
prev  = y_test.mean()
brier_max = prev * (1 - prev)                     # Brier of predicting prevalence for all
print("Brier:", brier, " scaled (prevalence-adjusted):", 1 - brier / brier_max)
```
   | Reading | Meaning |
   |---|---|
   | slope ≈ 1 and intercept ≈ 0 | well calibrated |
   | slope < 1 | predictions too extreme — overfitting/overconfidence |
   | slope > 1 | predictions too timid |
   | intercept ≠ 0 (slope ok) | systematic over/under-estimation of prevalence |
   | scaled Brier ≤ 0 | no better than predicting the prevalence |

4. **Recalibration** — only when step 3 is bad. **ASK first**, with this recommendation rule: fewer than ~1000 test-fold samples → Platt scaling; more → isotonic (more flexible).
```python
from sklearn.calibration import CalibratedClassifierCV
cal = CalibratedClassifierCV(model, method="sigmoid", cv=5)   # Platt; "isotonic" for isotonic
cal.fit(X_train, y_train)                                     # never calibrate on the test set
```
   Re-run ALL of step 3 on the recalibrated model and report before/after numbers.

5. **Decision threshold** (on request, for imbalance): sweep thresholds on the PR curve or maximize Youden J (sensitivity + specificity − 1) on a validation fold; report the chosen threshold and the step-2 metrics at that threshold.

6. **Feature importance / SHAP:**
   - Trees: `model.named_steps["clf"].feature_importances_` (pair with the post-encoding feature names from `pre.get_feature_names_out()`).
   - Any model (preferred): `permutation_importance(model, X_test, y_test, n_repeats=10, random_state=42)`; bar-plot the top 15, savefig.
   - **SHAP when requested** (`python3 -m pip install shap`):
```python
import shap
Xt = model.named_steps["pre"].transform(X_test)
explainer = shap.Explainer(model.named_steps["clf"], Xt)
sv = explainer(Xt)
shap.plots.beeswarm(sv, show=False); plt.savefig("ml_output/shap_beeswarm.png", bbox_inches="tight"); plt.close()
shap.plots.bar(sv, show=False);      plt.savefig("ml_output/shap_bar.png", bbox_inches="tight"); plt.close()
```

7. **Report template** — fill every line:
   - Data: file, n train / n test, label + positive class, CV scheme.
   - Baselines vs final model: one table (baseline, linear/logistic, final) on the same metrics.
   - Metrics: step 1 or 2 numbers, with CV mean ± std alongside the test-set value.
   - Calibration: intercept, slope, Brier, scaled Brier — and before/after if recalibrated.
   - Importance: top features + figure paths.
   - Decisions log: every ASK, the answer, and what was done.

## Rules

- Test-set metrics are computed once, after everything is frozen; tuning-time numbers are reported separately as "CV".
- Never recalibrate or threshold-tune on the test set — use CV folds or a validation split.
- Accuracy alone is banned on imbalanced data — always the step-2 panel plus PR-AUC.
- Run with `python3`; install `python3 -m pip install scikit-learn statsmodels shap`; figures via Agg + savefig only.
- R projects: tidymodels `yardstick` — `metric_set(rmse, mae, mape, rsq)` / `conf_mat`, `sens`, `spec`, `ppv`, `npv`, `roc_auc`; calibration via the `probably` package.
- Inference vs prediction: if the question is "which factors are associated with the outcome" (effects, CIs, p-values) rather than "predict it", use the `statistical-modeling` skill instead.
