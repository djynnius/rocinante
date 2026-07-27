---
name: nltk
description: "Classic NLP with NLTK: tokenization, stopwords, stemming and lemmatization, POS tagging, frequency distributions, collocations, n-grams, VADER sentiment analysis. Use when asked for text statistics, word frequencies, sentiment scoring, stemming, or corpus-style text analysis."
---

# NLTK

Classic/statistical NLP. Run with the `bash` tool (`python3`). The #1 failure is a missing data resource — every function needs its download (step 1); `LookupError: Resource X not found` names exactly what to download.

1. **Install + download the data the task needs** (quiet, scriptable):
```bash
python3 -m pip install nltk
python3 -c "
import nltk
for r in ['punkt_tab', 'stopwords', 'wordnet', 'averaged_perceptron_tagger_eng', 'vader_lexicon']:
    nltk.download(r, quiet=True)
print('ok')"
```
   | Function | Needs |
   |---|---|
   | `word_tokenize` / `sent_tokenize` | `punkt_tab` |
   | `stopwords.words("english")` | `stopwords` |
   | `WordNetLemmatizer` | `wordnet` |
   | `pos_tag` | `averaged_perceptron_tagger_eng` |
   | `SentimentIntensityAnalyzer` | `vader_lexicon` |

2. **Tokenize and clean** — the standard pipeline:
```python
from nltk.tokenize import word_tokenize, sent_tokenize
from nltk.corpus import stopwords

text = open("FILL_IN.txt").read()
sents = sent_tokenize(text)
tokens = [t.lower() for t in word_tokenize(text) if t.isalpha()]
stop = set(stopwords.words("english"))
content = [t for t in tokens if t not in stop]
print(len(sents), "sentences,", len(tokens), "tokens,", len(set(content)), "unique content words")
```

3. **Stem or lemmatize — pick deliberately:**
   | Want | Use | Example |
   |---|---|---|
   | Fast, crude root (search/matching) | `PorterStemmer().stem(w)` | "studies" → "studi" |
   | Real dictionary word (reports) | `WordNetLemmatizer().lemmatize(w, pos="v")` | "studies" → "study" |
   The lemmatizer defaults to noun — pass `pos="v"` for verbs or lemmas silently stay wrong.

4. **Frequencies, n-grams, collocations:**
```python
from nltk import FreqDist, bigrams
from nltk.collocations import BigramCollocationFinder, BigramAssocMeasures

fd = FreqDist(content)
print(fd.most_common(20))                       # top words
print(list(bigrams(content))[:10])              # adjacent pairs
finder = BigramCollocationFinder.from_words(content)
finder.apply_freq_filter(3)                     # seen at least 3 times
print(finder.nbest(BigramAssocMeasures.pmi, 15))  # phrases that belong together
```
   Plot: `fd.plot(20)` opens a window — instead use matplotlib Agg: bar-plot `fd.most_common(20)` and savefig.

5. **Sentiment with VADER** (tuned for social/informal text; compound ≥ 0.05 positive, ≤ −0.05 negative):
```python
from nltk.sentiment import SentimentIntensityAnalyzer
sia = SentimentIntensityAnalyzer()
df["sentiment"] = df["text"].astype(str).map(lambda t: sia.polarity_scores(t)["compound"])
print(df["sentiment"].describe())
```

6. **POS tagging**: `from nltk import pos_tag; pos_tag(word_tokenize("The old man the boat"))` — Penn Treebank tags (`NN` noun, `VB` verb, `JJ` adjective); `nltk.help.upenn_tagset("NN")` explains a tag.

## Rules

- Every `LookupError` is a missing `nltk.download("<named resource>")` — download it, do not reinstall the library.
- `mkdir -p nlp_output`; matplotlib Agg + savefig for every plot (`fd.plot` and `dispersion_plot` open windows — do not call them).
- VADER on formal/clinical text is unreliable — say so and treat scores as rough signal only.
- Lowercase + `isalpha` before frequency work, or punctuation and case variants pollute the counts.
- Modern pipelines (NER, dependency parses, batch speed) → the `spacy` skill; text features for a classifier → `TfidfVectorizer` + the `ml-modeling` skill.
