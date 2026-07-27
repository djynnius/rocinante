---
name: spacy
description: "NLP with spaCy: tokenization, part-of-speech tagging, lemmatization, named entity recognition (NER), dependency parsing, sentence splitting, similarity, batch processing. Use when asked to extract entities, tag or lemmatize text, parse sentences, or run production-style NLP over documents."
---

# spaCy

Industrial NLP pipelines. Run with the `bash` tool (`python3`). The #1 failure is a missing language model — always do step 1.

1. **Install the library AND a model** (two separate things):
```bash
python3 -m pip install spacy
python3 -m spacy download en_core_web_sm      # small English model, no word vectors
python3 -c "import spacy; spacy.load('en_core_web_sm'); print('ok')"
```
   Model choice: `en_core_web_sm` default; `en_core_web_md`/`lg` ONLY when similarity/vectors are needed (step 5). Other languages: `de_core_news_sm`, `fr_core_news_sm`, etc. `OSError: Can't find model` always means the download step was skipped.

2. **The core objects** — one pipeline, everything hangs off the `Doc`:
```python
import spacy
nlp = spacy.load("en_core_web_sm")
doc = nlp("Apple bought a startup in London for $2 billion last May.")

for token in doc:                       # tokens: text, lemma, POS, stopword flag
    print(token.text, token.lemma_, token.pos_, token.is_stop)
for ent in doc.ents:                    # named entities
    print(ent.text, ent.label_)         # Apple ORG · London GPE · $2 billion MONEY · last May DATE
for sent in doc.sents:                  # sentence splitting
    print(sent.text)
print(spacy.explain("GPE"))             # what a label means
```
   Attributes ending in `_` are strings (`pos_`, `lemma_`, `label_`); without `_` they are ints — always use the `_` form for output.

3. **Pick the task recipe:**
   | Need | Recipe |
   |---|---|
   | Clean tokens for downstream ML | `[t.lemma_.lower() for t in doc if not t.is_stop and not t.is_punct and t.is_alpha]` |
   | Entities per type | `[(e.text, e.label_) for e in doc.ents]`; filter `e.label_ == "PERSON"` |
   | Noun phrases | `[c.text for c in doc.noun_chunks]` |
   | Who-did-what (subject/verb/object) | `[(t.text, t.dep_, t.head.text) for t in doc]` — look for `nsubj`/`dobj` |
   | Rule-based phrase search | `spacy.matcher.PhraseMatcher` / `Matcher` with patterns |

4. **Many documents — always `nlp.pipe`, never a loop of `nlp(...)`:**
```python
texts = df["text"].astype(str).tolist()
docs = nlp.pipe(texts, batch_size=200, disable=["parser"])   # disable unused components = big speedup
rows = [{"text": d.text[:60], "ents": [(e.text, e.label_) for e in d.ents]} for d in docs]
```
   Disable what the task doesn't need: NER-only → `disable=["parser", "tagger", "lemmatizer"]`.

5. **Similarity** needs real vectors — `md`/`lg` model required (`sm` gives junk similarities and a warning):
```python
nlp = spacy.load("en_core_web_md")
print(nlp("dog").similarity(nlp("puppy")))    # ~0.8
```

6. **Visualize to a file** (never `displacy.serve` — it blocks the shell):
```python
from spacy import displacy
html = displacy.render(doc, style="ent", page=True)   # style="dep" for parse trees
open("nlp_output/entities.html", "w").write(html)
```

## Rules

- `mkdir -p nlp_output`; save visualizations to files and report paths; never `displacy.serve`.
- Model load once, outside loops; `nlp.pipe` for anything over ~20 texts.
- NER labels are model opinions, not truth — spot-check a sample before reporting entity counts as fact.
- Non-English text with an English model produces garbage silently — match the model to the language or say the language is unsupported.
- Classic/statistical NLP (stemming, corpora, VADER sentiment, collocations) → the `nltk` skill; text as features for a classifier → `sklearn` `TfidfVectorizer`, then the `ml-modeling` skill.
