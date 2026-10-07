import { useEffect, useState } from 'react';
import { PilotQuestion } from './PilotQuestion';
import { loadQuestionHistory, type Question } from './pilotQuestions';

export function PilotQuestionHistory({ session, turns }: { session: string; turns: string[] }) {
  const [open, setOpen] = useState(false);
  const [questions, setQuestions] = useState<Question[]>([]);
  const [loading, setLoading] = useState(false);
  const [problem, setProblem] = useState('');
  const [reload, setReload] = useState(0);
  const identifiers = JSON.stringify(turns);
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    setLoading(true); setProblem('');
    void loadQuestionHistory(session, JSON.parse(identifiers) as string[], controller.signal).then(result => {
      if (!controller.signal.aborted) setQuestions(result);
    }).catch(error => {
      if (!controller.signal.aborted) setProblem((error as Error).message);
    }).finally(() => {
      if (!controller.signal.aborted) setLoading(false);
    });
    return () => controller.abort();
  }, [session, identifiers, open, reload]);
  return <details className="pilot-tools" onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>历史提问</summary>
    {loading && <p className="pilot-muted" role="status">正在加载历史提问…</p>}
    {questions.map(question => <PilotQuestion key={`${question.turn}-${question.id}`} question={question} enabled={false} onAnswered={() => undefined} />)}
    {!loading && !problem && !questions.length && <p className="pilot-muted">没有已保存的历史提问。</p>}
    {problem && <p className="pilot-question-error" role="alert">无法加载历史提问：{problem}
      <button type="button" onClick={() => setReload(previous => previous + 1)}>重新加载</button>
    </p>}
  </details>;
}
