import { FC } from 'react';

// WPF keeps the operation log in the permanent right rail. Selecting this
// navigation item intentionally leaves the center workspace empty.
export const OperationLogPage: FC = () => (
  <section className="nw-operation-log-workspace" aria-label="操作日志工作区" />
);
