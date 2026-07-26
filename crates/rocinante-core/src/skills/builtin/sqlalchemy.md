---
name: sqlalchemy
description: "SQLAlchemy 2.x done right: engines, sessions, ORM models, queries with select(), transactions, relationships, avoiding N+1. Use when asked to write or fix SQLAlchemy code, define database models in Python, query or migrate a database from Python, or debug session/ORM errors."
---

# SQLAlchemy (2.x style)

Use 2.x style ONLY: `select()` statements executed by a session — never the legacy `session.query(...)`. Run code with the `bash` tool (`python3`); install with `python3 -m pip install sqlalchemy`.

1. **Engine and session** — one engine per app, short-lived sessions:
```python
from sqlalchemy import create_engine, select
from sqlalchemy.orm import Session

engine = create_engine("sqlite:///app.db")          # or postgresql+psycopg://user:pass@host/db
# echo=True prints every SQL statement — turn on when debugging
```

2. **Models:**
```python
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship

class Base(DeclarativeBase): ...

class User(Base):
    __tablename__ = "users"
    id: Mapped[int] = mapped_column(primary_key=True)
    name: Mapped[str]
    email: Mapped[str | None]                        # Optional = nullable
    posts: Mapped[list["Post"]] = relationship(back_populates="author")

class Post(Base):
    __tablename__ = "posts"
    id: Mapped[int] = mapped_column(primary_key=True)
    title: Mapped[str]
    user_id: Mapped[int] = mapped_column(ForeignKey("users.id"))
    author: Mapped["User"] = relationship(back_populates="posts")

Base.metadata.create_all(engine)                     # dev only; real projects use Alembic
```

3. **Write** — `Session.begin()` commits on success, rolls back on exception:
```python
with Session(engine) as session, session.begin():
    session.add(User(name="ada", email="ada@example.com"))
```

4. **Read:**
```python
with Session(engine) as session:
    users = session.scalars(select(User).where(User.name == "ada")).all()
    one   = session.get(User, 1)                     # by primary key
    rows  = session.execute(
        select(User.name, func.count(Post.id))
        .join(Post).group_by(User.name)
    ).all()                                          # tuples for multi-column selects
```
   `scalars()` for whole objects; `execute()` for column tuples.

5. **Update / delete** (load-then-change is simplest and runs ORM events):
```python
with Session(engine) as session, session.begin():
    user = session.get(User, 1)
    user.email = "new@example.com"                   # tracked automatically
    session.delete(session.get(User, 2))
```

6. **N+1 rule.** Accessing a relationship inside a loop fires one query per row. If a loop touches `obj.relationship`, add eager loading:
```python
from sqlalchemy.orm import selectinload
users = session.scalars(select(User).options(selectinload(User.posts))).all()
```

7. **Raw SQL** only via `text()` with bound parameters — NEVER f-strings (SQL injection):
```python
from sqlalchemy import text
session.execute(text("SELECT * FROM users WHERE name = :n"), {"n": name})
```

## Rules

- 2.x style only: `select()` + `session.scalars/execute`. If you see `session.query(...)` in existing code, match it for small fixes but write new code in 2.x style.
- Schema changes in a real project → Alembic (`alembic init`, `alembic revision --autogenerate -m "…"`, `alembic upgrade head`), not `create_all`.
- "DetachedInstanceError": the object outlived its session — access relationships inside the `with Session(...)` block or eager-load them (step 6).
- "Table already defined": the models module was imported twice; define models once, import from there.
- Debugging wrong SQL: recreate the engine with `echo=True` and read the statements actually sent.
- For the SQL itself (joins, windows, optimization) call the `skill` tool with `{"name": "sql-analytics"}`.
