/*
 * Differential oracle for the prompt history (rust/app/src/prompt.rs): addcmdtohist,
 * getcmdfromhist, xread_history, xwrite_history and xconfirm, called through nnn.c itself.
 * Same flags as build.sh; the Rust side is rust/app/src/bin/oracle_prompt.rs.
 *
 * Protocol, one op per line, tab-separated, strings in hex; one output line per op:
 *   A <hex>   addcmdtohist          G <0|1>  getcmdfromhist(up)
 *   S         selcmdpos = INVALID   X        clear the history
 *   R <hex>   xread_history(path)   W <hex>  xwrite_history(path), then the file in hex
 *   C <int>   xconfirm
 * After A/G/S/X/R/W the state follows: lastcmdpos selcmdpos entry,entry,...
 */
#define main nnn_main
#include "../../reference/nnn/src/nnn.c"
#undef main

static size_t unhex(const char *h, char *out)
{
	size_t n = 0;
	for (; h[0] && h[1]; h += 2)
		out[n++] = (char)((xchartohex((uchar_t)h[0]) << 4) | xchartohex((uchar_t)h[1]));
	out[n] = '\0';
	return n;
}

static void puthex(const char *s, size_t n)
{
	for (size_t i = 0; i < n; ++i)
		printf("%02x", (uchar_t)s[i]);
}

static void state(void)
{
	printf(" %d %d ", lastcmdpos, selcmdpos);
	if (lastcmdpos != INVALID_POS)
		for (int i = 0; i <= lastcmdpos; ++i) {
			if (i)
				putchar(',');
			puthex(cmd_hist[i], strlen(cmd_hist[i]));
		}
}

int main(void)
{
	static char line[1 << 16], arg[1 << 15];
	setlocale(LC_ALL, "");
	while (fgets(line, sizeof(line), stdin)) {
		line[strcspn(line, "\n")] = '\0';
		char op = line[0];
		const char *a = line[1] == '\t' ? line + 2 : "";
		unhex(a, arg);
		printf("%c", op);
		switch (op) {
		case 'A': addcmdtohist(arg); break;
		case 'G': {
			char *c = getcmdfromhist(a[0] == '1');
			putchar(' ');
			if (c)
				puthex(c, strlen(c));
			else
				putchar('-');
			break;
		}
		case 'S': selcmdpos = INVALID_POS; break;
		case 'X':
			if (lastcmdpos != INVALID_POS)
				for (int i = 0; i <= lastcmdpos; ++i)
					free(cmd_hist[i]);
			lastcmdpos = selcmdpos = INVALID_POS;
			break;
		case 'R': xread_history(arg); break;
		case 'W': {
			fflush(stdout);
			xwrite_history(arg);
			fflush(NULL); /* the original flushes at exit; read it back now */
			FILE *f = fopen(arg, "r");
			putchar(' ');
			if (f) {
				int ch;
				while ((ch = fgetc(f)) != EOF)
					printf("%02x", ch);
				fclose(f);
			} else
				putchar('-');
			break;
		}
		case 'C': printf(" %d\n", xconfirm(atoi(a))); fflush(stdout); continue;
		}
		state();
		putchar('\n');
		fflush(stdout);
	}
	return 0;
}
