import { FC, ReactNode } from 'react';

type ModalLayerProps = {
  isVisible: boolean;
  title?: string | null;
  children?: ReactNode;
  onClose?: () => void;
};

export const ModalLayer: FC<ModalLayerProps> = ({ isVisible, title, children, onClose }) =>
  !isVisible ? null : (
    <section className="nw-modal-layer" aria-live="polite">
      <div className="nw-modal-backdrop" />
      <div className="nw-modal-card" role="dialog" aria-label={title || '模态层'}>
        {title ? <header className="nw-modal-title">{title}</header> : null}
        <div className="nw-modal-body">{children}</div>
        {onClose ? (
          <button type="button" className="nw-modal-close" onClick={onClose}>
            关闭
          </button>
        ) : null}
      </div>
    </section>
  );
