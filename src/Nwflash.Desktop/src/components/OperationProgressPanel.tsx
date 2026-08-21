import { FC } from 'react';

type OperationProgressPanelProps = {
  progressText: string;
};

export const OperationProgressPanel: FC<OperationProgressPanelProps> = ({ progressText }) => (
  <section className="nw-progress-panel" data-role="operation-progress">
    <div className="nw-progress-title">操作进度</div>
    <div className="nw-progress-text">{progressText}</div>
  </section>
);
