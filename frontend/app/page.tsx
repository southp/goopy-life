export default function Home() {
  return (
    <div className="flex min-h-screen items-center justify-center bg-zinc-50 font-sans dark:bg-black">
      <main className="flex min-h-screen w-full max-w-3xl flex-col items-center justify-center gap-4 bg-white dark:bg-black">
		  <span className="text-9xl hue-rotate-180 brightness-150 opacity-85 [text-shadow:0_0_20px_rgba(255,255,255,0.8)]">
			💩
		  </span>
          <a
            className="flex h-12 w-full items-center justify-center gap-2 rounded-full bg-foreground px-5 text-background transition-colors hover:bg-[#383838] dark:hover:bg-[#ccc] md:w-[158px]"
            href="https://vercel.com/new?utm_source=create-next-app&utm_medium=appdir-template-tw&utm_campaign=create-next-app"
            target="_blank"
            rel="noopener noreferrer"
          >
            Ghost now!
          </a>
      </main>
    </div>
  );
}
