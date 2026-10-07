use std::collections::BTreeMap;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    Question, QuestionAnswer, QuestionStatus, Session, SessionError, SessionStatus, SessionStore,
    Turn, TurnStatus, question_schema, store, turns, validate_question_schema,
};

const COLUMNS: &str = "questions.id, turns.session_id, questions.turn_id, questions.message,
    questions.requested_schema, questions.status, questions.answer";

impl SessionStore {
    pub fn create_question(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        message: &str,
        requested_schema: Value,
    ) -> Result<Question, SessionError> {
        turns::validate_turn_lookup(owner, session, turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        ensure_live(&transaction, session, turn)?;
        if message.trim().is_empty() || message.len() > question_schema::MAX_FORM_BYTES {
            return Err(SessionError::Invalid(
                "question message must be nonblank and at most 64 KiB".into(),
            ));
        }
        validate_question_schema(&requested_schema)?;
        let pending: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM questions WHERE turn_id = ?1 AND status = 'pending')",
            [turn],
            |row| row.get(0),
        )?;
        if pending {
            return Err(SessionError::Conflict(
                "Turn already has a pending question".into(),
            ));
        }
        let question = Question {
            id: Uuid::new_v4().to_string(),
            session: session.into(),
            turn: turn.into(),
            message: message.into(),
            requested_schema,
            status: QuestionStatus::Pending,
            answer: None,
        };
        transaction.execute(
            "INSERT INTO questions(id, turn_id, seq, message, requested_schema, status, answer)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(seq), 0) + 1 FROM questions WHERE turn_id = ?2),
                     ?3, ?4, 'pending', NULL)",
            params![
                question.id,
                turn,
                message,
                serde_json::to_string(&question.requested_schema)?
            ],
        )?;
        snapshot.status = SessionStatus::WaitingUser;
        snapshot.waiting_reason = message.into();
        snapshot.updated_at = Utc::now();
        store::write_session(&transaction, owner, &snapshot)?;
        append_question_event(&transaction, &snapshot, &question, "question")?;
        transaction.commit()?;
        Ok(question)
    }

    pub fn get_question(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        question: &str,
    ) -> Result<Question, SessionError> {
        validate_lookup(owner, session, turn, question)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        let question = find_question(&transaction, session, turn, question)?;
        transaction.commit()?;
        Ok(question)
    }

    pub fn list_questions(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
    ) -> Result<Vec<Question>, SessionError> {
        turns::validate_turn_lookup(owner, session, turn)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        store::read_session(&transaction, owner, session)?;
        turns::find_turn(&transaction, session, turn)?;
        let questions = query_questions(&transaction, session, turn, false)?;
        transaction.commit()?;
        Ok(questions)
    }

    pub fn answer_question(
        &self,
        owner: &str,
        session: &str,
        turn: &str,
        question: &str,
        answer: QuestionAnswer,
    ) -> Result<Question, SessionError> {
        validate_lookup(owner, session, turn, question)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = store::read_session(&transaction, owner, session)?;
        let mut question = find_question(&transaction, session, turn, question)?;
        if question.status == QuestionStatus::Answered && question.answer.as_ref() == Some(&answer)
        {
            transaction.commit()?;
            return Ok(question);
        }
        if question.status != QuestionStatus::Pending {
            return Err(SessionError::Conflict(
                "question is no longer pending".into(),
            ));
        }
        ensure_live(&transaction, session, turn)?;
        question_schema::validate_answer(&question.requested_schema, &answer)?;
        let encoded = question_schema::bounded_json(&answer, "answer")?;
        let updated = transaction.execute(
            "UPDATE questions SET status = 'answered', answer = ?2 WHERE id = ?1 AND status = 'pending'",
            params![question.id, encoded],
        )?;
        if updated != 1 {
            return Err(SessionError::Conflict(
                "question is no longer pending".into(),
            ));
        }
        question.status = QuestionStatus::Answered;
        question.answer = Some(answer);
        snapshot.status = SessionStatus::Active;
        snapshot.waiting_reason.clear();
        snapshot.updated_at = Utc::now();
        store::write_session(&transaction, owner, &snapshot)?;
        append_question_event(&transaction, &snapshot, &question, "question-answered")?;
        transaction.commit()?;
        Ok(question)
    }
}

pub(crate) fn interrupt_pending(
    connection: &Connection,
    session: &Session,
    turn: &Turn,
) -> Result<(), SessionError> {
    for mut question in query_questions(connection, &session.id, &turn.id, true)? {
        connection.execute(
            "UPDATE questions SET status = 'interrupted' WHERE id = ?1 AND status = 'pending'",
            [&question.id],
        )?;
        question.status = QuestionStatus::Interrupted;
        append_question_event(connection, session, &question, "question-interrupted")?;
    }
    Ok(())
}

fn append_question_event(
    connection: &Connection,
    session: &Session,
    question: &Question,
    kind: &str,
) -> Result<(), SessionError> {
    turns::append_ui_event(
        connection,
        &question.turn,
        json!({"type": kind, "question": question}),
    )?;
    store::append_event(
        connection,
        session,
        kind,
        BTreeMap::from([("question".into(), json!(question))]),
    )
}

fn ensure_live(connection: &Connection, session: &str, turn: &str) -> Result<(), SessionError> {
    if turns::find_turn(connection, session, turn)?.status != TurnStatus::Running {
        return Err(SessionError::Conflict(
            "question requires a live running Turn".into(),
        ));
    }
    Ok(())
}

fn validate_lookup(
    owner: &str,
    session: &str,
    turn: &str,
    question: &str,
) -> Result<(), SessionError> {
    turns::validate_turn_lookup(owner, session, turn)?;
    store::validate_identity(question, "question id")
}

fn find_question(
    connection: &Connection,
    session: &str,
    turn: &str,
    question: &str,
) -> Result<Question, SessionError> {
    connection
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM questions JOIN turns ON turns.id = questions.turn_id
                WHERE turns.session_id = ?1 AND questions.turn_id = ?2 AND questions.id = ?3"
            ),
            params![session, turn, question],
            read_question,
        )
        .optional()?
        .ok_or(SessionError::Missing)?
}

fn query_questions(
    connection: &Connection,
    session: &str,
    turn: &str,
    pending_only: bool,
) -> Result<Vec<Question>, SessionError> {
    let filter = if pending_only {
        " AND questions.status = 'pending'"
    } else {
        ""
    };
    let mut statement = connection.prepare(&format!(
        "SELECT {COLUMNS} FROM questions JOIN turns ON turns.id = questions.turn_id
         WHERE turns.session_id = ?1 AND questions.turn_id = ?2{filter} ORDER BY questions.seq"
    ))?;
    statement
        .query_map(params![session, turn], read_question)?
        .map(|row| {
            row.map_err(SessionError::from)
                .and_then(|question| question)
        })
        .collect()
}

fn read_question(row: &Row<'_>) -> rusqlite::Result<Result<Question, SessionError>> {
    let schema: String = row.get(4)?;
    let status: String = row.get(5)?;
    let answer: Option<String> = row.get(6)?;
    let fields = (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
    Ok((|| {
        let status = match status.as_str() {
            "pending" => QuestionStatus::Pending,
            "answered" => QuestionStatus::Answered,
            "interrupted" => QuestionStatus::Interrupted,
            _ => {
                return Err(SessionError::Storage(
                    "invalid stored question status".into(),
                ));
            }
        };
        Ok(Question {
            id: fields.0,
            session: fields.1,
            turn: fields.2,
            message: fields.3,
            requested_schema: serde_json::from_str(&schema)?,
            status,
            answer: answer
                .map(|answer| serde_json::from_str(&answer))
                .transpose()?,
        })
    })())
}
