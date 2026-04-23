import "./globals.css";

export const metadata = {
  title: "Next Counter",
  description: "Benchmark counter fixture for Next.js",
};

export default function RootLayout({ children }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
