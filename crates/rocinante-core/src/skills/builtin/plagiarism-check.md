---
name: plagiarism-check
description: Read a text and flag passages that read as unattributed borrowing or too close to a likely source, and identify claims, quotes, and data that need a citation. Use when the user asks to check writing for plagiarism, originality, or missing attribution. This is a reasoning-based aid, not a database-backed scanner.
---

# Plagiarism check

Find citation gaps and suspicious phrasing by reasoning about the text. Be honest about the limits.

1. Read the target text closely.
2. Flag passages that read as unattributed borrowing: sudden shifts in voice or register, phrasing more polished than the surrounding text, boilerplate definitions, or wording that sounds lifted from a known source.
3. Identify every specific claim, statistic, quotation, or piece of data that asserts a fact but carries no citation — these need attribution regardless of wording.
4. Where web access or `agent: miller` is available, spot-check the most suspicious passages against likely sources with a `task` call, and report any close matches you find.
5. Report per flag: the passage (quote it), why it is suspicious or under-cited, and what the author should do (cite, quote and attribute, or reword).

Rules:
- BE HONEST about scope: this is a reasoning-based aid, NOT a database-backed plagiarism scanner. It surfaces citation gaps and suspicious phrasing — it cannot produce definitive matches or a similarity score.
- Do not accuse. Frame findings as "needs attribution" or "verify against source," not verdicts.
- If `miller` and web are not available, do the reasoning pass yourself and clearly note that no source comparison was performed.
