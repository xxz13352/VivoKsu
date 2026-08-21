import { FC } from 'react';
import { ModalLayer } from './ModalLayer';

export type UpdateRequiredDetails = {
  message: string;
  latest: string | null;
  minVersion: string | null;
  downloadUrl: string | null;
};

type UpdateRequiredDialogProps = {
  details: UpdateRequiredDetails | null;
  onQuit: () => void;
};

export const UpdateRequiredDialog: FC<UpdateRequiredDialogProps> = ({ details, onQuit }) => (
  <ModalLayer isVisible={details !== null} title="奶蛙Flash 需要更新">
    {details ? (
      <section className="nw-update-required" aria-label="版本更新要求">
        <p className="nw-page-eyebrow">SECURE GATE · 版本门禁</p>
        <h2>需要更新 奶蛙Flash</h2>
        <p>{details.message}</p>
        <dl>
          <div>
            <dt>最新版本</dt>
            <dd>{details.latest || '未知'}</dd>
          </div>
          <div>
            <dt>最低要求</dt>
            <dd>{details.minVersion || '未配置'}</dd>
          </div>
        </dl>
        <div className="nw-update-required-actions">
          {details.downloadUrl ? (
            <a
              href={details.downloadUrl}
              target="_blank"
              rel="noreferrer"
              aria-label="下载新版本"
              onClick={onQuit}
            >
              下 载 新 版 本
            </a>
          ) : (
            <button type="button" aria-label="下载新版本" disabled>
              未配置下载链接
            </button>
          )}
          <button type="button" onClick={onQuit}>退 出</button>
        </div>
      </section>
    ) : null}
  </ModalLayer>
);
