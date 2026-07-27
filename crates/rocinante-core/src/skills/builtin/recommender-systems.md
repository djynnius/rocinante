---
name: recommender-systems
description: "Recommendations and market-basket analysis: association rules with Apriori, FP-Growth, and Eclat (support, confidence, lift), plus collaborative filtering (item-based, matrix factorization). Use when asked what items are bought together, to mine association rules, build a recommender, or suggest items to users."
---

# Recommender Systems

Two families — pick by the data you have. Run with the `bash` tool (`python3`). At every **ASK** gate: ask the user with your recommendation; as a subagent, stop and return the question + recommendation in your report.

| You have | Family | Go to |
|---|---|---|
| Transactions/baskets (receipt lines, carts) | Association rules (Apriori / FP-Growth / Eclat) | steps 1–4 |
| User–item ratings or interactions | Collaborative filtering | step 5 |

1. **Shape the transactions** into one-hot basket format (rows = transactions, columns = items, values True/False):
```python
import pandas as pd
from mlxtend.preprocessing import TransactionEncoder     # python3 -m pip install mlxtend

df = pd.read_csv("FILL_IN.csv")                          # e.g. columns: transaction_id, item
baskets = df.groupby("transaction_id")["item"].apply(list).tolist()
te = TransactionEncoder()
onehot = pd.DataFrame(te.fit(baskets).transform(baskets), columns=te.columns_)
print(onehot.shape, "transactions x items")
```

2. **Mine frequent itemsets.** Apriori, FP-Growth, and Eclat find the SAME itemsets — they differ in speed and library:
   | Algorithm | Use | When |
   |---|---|---|
   | Apriori | `mlxtend.frequent_patterns.apriori` | small/medium data, the classic, easiest to explain |
   | FP-Growth | `mlxtend.frequent_patterns.fpgrowth` | larger data — much faster, same results (recommend by default) |
   | Eclat | `pyECLAT` (`python3 -m pip install pyECLAT`), or use FP-Growth as the efficient equivalent | when Eclat is specifically requested |
```python
from mlxtend.frequent_patterns import fpgrowth           # or: apriori
itemsets = fpgrowth(onehot, min_support=0.01, use_colnames=True)
print(itemsets.sort_values("support", ascending=False).head(20))
```
   `min_support` gate: start at 0.01 (item combo in ≥1% of transactions); zero results → halve it; thousands of results → raise it. **ASK** if a domain-specific threshold exists.
   Eclat when demanded by name:
```python
from pyECLAT import ECLAT
ec = ECLAT(data=pd.DataFrame(baskets))                   # rows = transactions, items in columns
idx, supports = ec.fit(min_support=0.01, min_combination=2, max_combination=3, verbose=False)
print(sorted(supports.items(), key=lambda kv: -kv[1])[:20])
```

3. **Turn itemsets into rules** and read the three numbers right:
```python
from mlxtend.frequent_patterns import association_rules
rules = association_rules(itemsets, metric="lift", min_threshold=1.2)
cols = ["antecedents", "consequents", "support", "confidence", "lift"]
print(rules.sort_values("lift", ascending=False)[cols].head(15))
```
   | Metric | Formula | Meaning | Gate |
   |---|---|---|---|
   | support | P(A∧B) | how common the combo is | already filtered in step 2 |
   | confidence | P(B\|A) | how often A leads to B | report, don't filter alone — popular B inflates it |
   | lift | conf / P(B) | how much A boosts B vs chance | **> 1.2 interesting; ≈ 1 coincidence; < 1 negative** |
   Report rules as sentences: "Customers buying {A} are LIFTx more likely to also buy {B} (seen in SUPPORT% of transactions)."

4. **Recommend from rules**: for a given basket, match rules whose antecedents ⊆ basket, rank consequents by lift, drop items already in the basket. Report the top N with their lift.

5. **Collaborative filtering** (ratings/interactions matrix):
   - Item-based, no extra deps — the robust default:
```python
from sklearn.metrics.pairwise import cosine_similarity
ui = df.pivot_table(index="user_id", columns="item_id", values="rating").fillna(0)
sim = pd.DataFrame(cosine_similarity(ui.T), index=ui.columns, columns=ui.columns)
def recommend(user, n=5):
    seen = ui.loc[user][ui.loc[user] > 0]
    scores = (sim[seen.index] * seen.values).sum(axis=1).drop(seen.index)
    return scores.nlargest(n)
```
   - Matrix factorization (`scikit-surprise` SVD) for real rating prediction — needs a compiler to install; mention, use only when item-based is insufficient.
   - Evaluate with a leave-last-out split and **precision@k / hit-rate@k** — never accuracy. Popularity baseline (recommend the top sellers) must be beaten to claim the model works.

## Rules

- Install: `python3 -m pip install mlxtend pandas scikit-learn` (+ `pyECLAT` only if Eclat is demanded by name).
- One-hot columns must be boolean (True/False), not counts — mlxtend warns, then misbehaves.
- Rules are correlations, not causation — never phrase them as "buying A causes B".
- Sparse giant baskets (millions of rows): aggregate in DuckDB first (`skill` tool `{"name": "duckdb"}`) before one-hot encoding, or memory dies.
- Cold-start (new user/item) has no CF answer — fall back to popularity or rules, and say so.
- Model-selection questions beyond this (classification/regression on features) → `{"name": "ml-modeling"}`.
