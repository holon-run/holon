import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { memo } from "react";

interface MarkdownContentProps {
  text: string;
  compact?: boolean;
}

function MarkdownContentView({ text, compact = false }: MarkdownContentProps) {
  return (
    <div className={`markdown-content${compact ? " compact" : ""}`}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ children, ...props }) => (
            <a {...props} rel="noreferrer" target="_blank">
              {children}
            </a>
          ),
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}

export const MarkdownContent = memo(MarkdownContentView);
