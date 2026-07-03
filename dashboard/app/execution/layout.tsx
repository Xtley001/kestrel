import { Sidebar } from "@/components/Sidebar";
export default function Layout({ children }: { children: React.ReactNode }) {
  return <div className="flex min-h-screen"><Sidebar /><main className="flex-1 overflow-auto p-6">{children}</main></div>;
}
