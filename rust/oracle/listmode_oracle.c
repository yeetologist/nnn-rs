/*
 * Differential oracle for list mode: runs nnn.c's own load_input() (and so make_tmp_tree)
 * on this process's stdin. Built like oracle.c (see build.sh flags).
 *
 *   listmode_oracle <tmp-dir> [<cwd-for-relative-paths>]
 *
 * Prints, one per line, "ret", "listroot" and "listpath" followed by the value in hex or
 * NULL. Messages go to stderr (msg(): home is still unset, as at startup).
 * rust/app/src/bin/oracle_listmode.rs speaks the same protocol; listmode_run.py compares.
 */
#define main nnn_main
#include "../../reference/nnn/src/nnn.c"
#undef main

static void put(const char *key, const char *s)
{
	printf("%s ", key);
	if (!s) {
		puts("NULL");
		return;
	}
	for (; *s; ++s)
		printf("%02x", (uchar_t)*s);
	putchar('\n');
}

int main(int argc, char **argv)
{
	if (argc < 2)
		return 2;
	tmpfplen = (uchar_t)xstrsncpy(g_tmpfpath, argv[1], TMP_LEN_MAX);
	char *ret = load_input(STDIN_FILENO, (argc > 2 && argv[2][0]) ? argv[2] : NULL);
	put("ret", ret);
	put("listroot", listroot);
	put("listpath", listpath);
	return 0;
}
