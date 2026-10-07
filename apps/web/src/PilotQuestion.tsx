import { useEffect, useRef, useState, type FormEvent } from 'react';
import { answerQuestion, questionContent, questionFields, type Question, type QuestionAnswer } from './pilotQuestions';
import './PilotQuestion.css';

const answers = { accept: '已回答', decline: '已拒绝', cancel: '已取消' };
export function PilotQuestion({ question, enabled, stopping = false, onAnswered }: {
  question: Question; enabled: boolean; stopping?: boolean; onAnswered: (question: Question) => void;
}) {
  const [values, setValues] = useState<Record<string, string>>({});
  const [submitting, setSubmitting] = useState(false);
  const [problem, setProblem] = useState('');
  const submission = useRef<AbortController | null>(null);
  const allowed = useRef(false);
  const active = question.status === 'pending' && enabled;
  allowed.current = active;
  useEffect(() => {
    allowed.current = active;
    if (!active) { submission.current?.abort(); setSubmitting(false); }
    return () => { allowed.current = false; submission.current?.abort(); };
  }, [active]);
  const form = questionFields(question.requested_schema);
  const answer = async (action: QuestionAnswer['action']) => {
    if (!allowed.current || submission.current) return;
    let content: Record<string, unknown> | undefined;
    try {
      if (action === 'accept') {
        if (form.error) throw new Error(form.error);
        content = questionContent(form.fields!, values);
      }
    } catch (error) { setProblem((error as Error).message); return; }
    const controller = new AbortController();
    submission.current = controller;
    setSubmitting(true); setProblem('');
    try {
      const result = await answerQuestion(question, {
        action, ...(content === undefined ? {} : { content }),
      }, controller.signal);
      if (!controller.signal.aborted && allowed.current) onAnswered(result);
    } catch (error) {
      if (!controller.signal.aborted && allowed.current) setProblem((error as Error).message);
    } finally {
      if (submission.current === controller) submission.current = null;
      if (!controller.signal.aborted && allowed.current) setSubmitting(false);
    }
  };
  const submit = (event: FormEvent) => { event.preventDefault(); void answer('accept'); };
  return <section className="pilot-question" aria-label="Pilot 提问">
    <p className="pilot-user-text">{question.message}</p>
    {question.status === 'pending' && active ? <form onSubmit={submit} noValidate>
      {form.error ? <p className="pilot-muted" role="status">{form.error}</p> : form.fields!.map(field => {
        const id = `question-${question.id}-${field.name}`;
        const common = { id, name: field.name, value: Object.hasOwn(values, field.name) ? values[field.name] : '', disabled: submitting, required: field.required,
          'aria-describedby': field.description ? `${id}-description` : undefined,
          onChange: (event: { target: { value: string } }) => setValues(previous => ({ ...previous, [field.name]: event.target.value })) };
        return <div className="pilot-question-field" key={field.name}>
          <label htmlFor={id}>{field.title}{field.required && ' *'}</label>
          {field.enum ? <select {...common}><option value="">请选择</option>
            {field.enum.map((value, index) => <option value={String(index)} key={index}>{typeof value === 'boolean' ? value ? '是' : '否' : String(value)}</option>)}
          </select> : field.type === 'boolean' ? <select {...common}><option value="">请选择</option><option value="true">是</option><option value="false">否</option></select> :
            <input {...common} type={field.type === 'string' ? 'text' : 'number'} minLength={field.minLength} maxLength={field.maxLength}
              min={field.minimum} max={field.maximum} step={field.type === 'integer' ? 1 : field.multipleOf ?? 'any'} />}
          {field.description && <small id={`${id}-description`}>{field.description}</small>}
        </div>;
      })}
      {problem && <p className="pilot-question-error" role="alert">{problem}</p>}
      <div className="pilot-question-actions">
        <button type="button" disabled={submitting} onClick={() => void answer('decline')}>拒绝</button>
        <button type="button" disabled={submitting} onClick={() => void answer('cancel')}>取消</button>
        <button type="submit" className="primary" disabled={submitting || !!form.error}>{submitting ? '正在提交…' : '回答'}</button>
      </div>
    </form> : <div>
      <p className="pilot-muted" role="status">{question.status === 'interrupted' ? '问题已中断，无法再回答。' : question.status === 'answered'
        ? question.answer ? answers[question.answer.action] : '已回答' : stopping ? '正在停止，不再接受回答。' : '本次执行已结束，无法再回答此问题。'}</p>
      {question.answer?.content && <dl className="pilot-question-answer">{Object.entries(question.answer.content).map(([name, value]) => <div key={name}>
        <dt>{form.fields?.find(field => field.name === name)?.title ?? name}</dt><dd>{typeof value === 'boolean' ? value ? '是' : '否' : String(value)}</dd>
      </div>)}</dl>}
    </div>}
  </section>;
}
