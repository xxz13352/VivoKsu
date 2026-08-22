import { errorMessage } from '../app/error';
import { FC, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

type ResourceInventoryItem = {
  key: string;
  display_name: string;
  is_ready: boolean;
  default_selected: boolean;
};

type ResourceDownloadPageProps = {
  onCompleted?: () => void;
  onRequestClose?: () => void;
  onInstallingChange?: (installing: boolean) => void;
  embedded?: boolean;
};

const normalizeInventory = (value: unknown): ResourceInventoryItem[] =>
  Array.isArray(value)
    ? value.filter(
        (item): item is ResourceInventoryItem =>
          Boolean(item) &&
          typeof item === 'object' &&
          typeof (item as Record<string, unknown>).key === 'string' &&
          typeof (item as Record<string, unknown>).display_name === 'string' &&
          typeof (item as Record<string, unknown>).is_ready === 'boolean' &&
          typeof (item as Record<string, unknown>).default_selected === 'boolean',
      )
    : [];

export const ResourceDownloadPage: FC<ResourceDownloadPageProps> = ({
  onCompleted,
  onRequestClose,
  onInstallingChange,
  embedded = false,
}) => {
  const [items, setItems] = useState<ResourceInventoryItem[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [installing, setInstalling] = useState(false);
  const [errorText, setErrorText] = useState('');

  const loadInventory = async (): Promise<boolean> => {
    setLoading(true);
    setErrorText('');
    try {
      const response = await invoke<unknown>('resource_inventory');
      const nextItems = normalizeInventory(response);
      setItems(nextItems);
      setSelected(new Set(nextItems.filter((item) => item.default_selected).map((item) => item.key)));
      return true;
    } catch (error) {
      setItems([]);
      setSelected(new Set());
      setErrorText(errorMessage(error, '内置资源状态读取失败'));
      return false;
    } finally {
      setLoading(false);
    }
  };

  const installSelected = async () => {
    const keys = items.filter((item) => selected.has(item.key) && !item.is_ready).map((item) => item.key);
    if (keys.length === 0) {
      return;
    }

    setInstalling(true);
    onInstallingChange?.(true);
    setErrorText('');
    try {
      await invoke<string[]>('resource_install', { keys });
      if (await loadInventory()) {
        onCompleted?.();
      }
    } catch (error) {
      setErrorText(errorMessage(error, '内置资源校验失败'));
    } finally {
      setInstalling(false);
      onInstallingChange?.(false);
    }
  };

  const requestClose = async () => {
    if (installing) {
      try {
        await invoke('operation_cancel');
      } finally {
        onRequestClose?.();
      }
      return;
    }
    onRequestClose?.();
  };

  useEffect(() => {
    void loadInventory();
  }, []);

  const selectedMissingCount = items.filter((item) => selected.has(item.key) && !item.is_ready).length;

  return (
    <section className={embedded ? 'nw-resource-download-content' : 'nw-card'}>
      {!embedded ? <h2>内置组件检查</h2> : null}
      {loading ? <p>正在检测组件...</p> : null}
      {errorText ? <p className="nw-error-text">{errorText}</p> : null}
      <ul className="nw-resource-list">
        {items.map((item) => (
          <li key={item.key}>
            <label>
              <input
                type="checkbox"
                data-resource-key={item.key}
                checked={selected.has(item.key)}
                disabled={item.is_ready || installing}
                onChange={(event) => {
                  setSelected((current) => {
                    const next = new Set(current);
                    if (event.currentTarget.checked) next.add(item.key);
                    else next.delete(item.key);
                    return next;
                  });
                }}
              />
              {item.display_name}：{item.is_ready ? '已就绪' : '内置缺失'}
            </label>
          </li>
        ))}
      </ul>
      <div className="nw-resource-actions">
        <button
          type="button"
          className="nw-test-resource-install"
          disabled={loading || installing || selectedMissingCount === 0}
          onClick={() => void installSelected()}
        >
          {installing ? '校验中...' : `校验所选 (${selectedMissingCount})`}
        </button>
        {onRequestClose ? (
          <button type="button" className="nw-test-resource-close" onClick={() => void requestClose()}>
            {installing ? '取消校验' : '关闭'}
          </button>
        ) : null}
      </div>
    </section>
  );
};
