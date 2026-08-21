import { FC } from 'react';

type PageContainerProps = {
  children: React.ReactNode;
  flushTop?: boolean;
};

export const PageContainer: FC<PageContainerProps> = ({ children, flushTop = false }) => (
  <section className={`nw-page-content${flushTop ? ' nw-page-content-flush-top' : ''}`}>
    {children}
  </section>
);
