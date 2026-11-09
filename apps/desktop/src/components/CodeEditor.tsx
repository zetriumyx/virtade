import { useRef, useEffect } from 'react';
import * as monaco from 'monaco-editor';
import Editor, { DiffEditor, loader } from '@monaco-editor/react';
import EditorWorker from 'monaco-editor/editor/editor.worker?worker';
import JsonWorker from 'monaco-editor/language/json/json.worker?worker';
import TsWorker from 'monaco-editor/language/typescript/ts.worker?worker';
(self as unknown as { MonacoEnvironment: unknown }).MonacoEnvironment = {
  getWorker: (_: unknown, label: string) =>
    label === 'json'
      ? new JsonWorker()
      : ['typescript', 'javascript'].includes(label)
        ? new TsWorker()
        : new EditorWorker(),
};
loader.config({ monaco });
function language(path: string) {
  const ext = path.split('.').pop() ?? '';
  return (
    (
      {
        rs: 'rust',
        ts: 'typescript',
        tsx: 'typescript',
        js: 'javascript',
        jsx: 'javascript',
        json: 'json',
        md: 'markdown',
        py: 'python',
        toml: 'ini',
        yaml: 'yaml',
        yml: 'yaml',
        css: 'css',
        html: 'html',
        sh: 'shell',
        sql: 'sql',
      } as Record<string, string>
    )[ext] ?? 'plaintext'
  );
}
export default function CodeEditor({
  path,
  value,
  onChange,
  onSave,
  theme,
  revision,
}: {
  revision: string;
  path: string;
  value: string;
  onChange: (value: string) => void;
  onSave: () => void;
  theme: 'dark' | 'light';
}) {
  const save = useRef(onSave);
  save.current = onSave;
  const instance = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  useEffect(() => {
    const model = instance.current?.getModel();
    if (model && model.getValue() !== value) model.setValue(value);
  }, [path, revision]);
  return (
    <div
      className="monaco-host"
      onKeyDownCapture={(event) => {
        if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
          event.preventDefault();
          event.stopPropagation();
          save.current();
        }
      }}
    >
      <Editor
        path={path}
        language={language(path)}
        defaultValue={value}
        theme={theme === 'dark' ? 'vs-dark' : 'vs'}
        onChange={(value) => onChange(value ?? '')}
        onMount={(editor) => {
          instance.current = editor;
          editor.focus();
        }}
        options={{
          fontFamily: 'Cascadia Code, SFMono-Regular, Consolas, monospace',
          fontSize: 13,
          minimap: { enabled: false },
          scrollBeyondLastLine: false,
          automaticLayout: true,
          tabSize: 2,
          padding: { top: 15 },
          renderWhitespace: 'selection',
        }}
      />
    </div>
  );
}
export function CodeDiff({
  path,
  original,
  modified,
  theme,
}: {
  path: string;
  original: string;
  modified: string;
  theme: 'dark' | 'light';
}) {
  return (
    <DiffEditor
      language={language(path)}
      original={original}
      modified={modified}
      theme={theme === 'dark' ? 'vs-dark' : 'vs'}
      options={{
        readOnly: true,
        automaticLayout: true,
        minimap: { enabled: false },
        renderSideBySide: true,
        fontSize: 12,
        scrollBeyondLastLine: false,
      }}
    />
  );
}
