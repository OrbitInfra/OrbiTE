/* The packaged consumer loads libibverbs with an explicit RTLD_LOCAL handle.
 * Observe only lookups on that handle, and forward calls to that same DSO. */
static void *(*system_lookup)(void *, const char *);
static pthread_once_t lookup_once = PTHREAD_ONCE_INIT, verbs_once = PTHREAD_ONCE_INIT;
static void *verbs_library;

static void resolve_lookup(void) {
#ifdef __aarch64__
    const char *version = "GLIBC_2.17";
#else
    const char *version = "GLIBC_2.2.5";
#endif
    *(void **)(&system_lookup) = dlvsym(RTLD_NEXT, "dlsym", version);
    if (!system_lookup) fail();
}

static void pin_verbs(void) {
    /* Saved callbacks remain valid until the qualification process exits. */
    verbs_library = dlopen("libibverbs.so.1", RTLD_LAZY | RTLD_NOLOAD);
    if (!verbs_library) fail();
}

static void *native_verb(const char *name) {
    pthread_once(&lookup_once, resolve_lookup);
    pthread_once(&verbs_once, pin_verbs);
    return system_lookup(verbs_library, name);
}

void *dlsym(void *library, const char *name) {
    pthread_once(&lookup_once, resolve_lookup);
    void *native = system_lookup(library, name);
    if (!native || library == RTLD_DEFAULT || library == RTLD_NEXT || strncmp(name, "ibv_", 4))
        return native;
    static const struct { const char *name; void *wrapper; } observed[] = {
#define OBSERVE(symbol) { #symbol, (void *)(symbol) }
        OBSERVE(ibv_open_device), OBSERVE(ibv_close_device), OBSERVE(ibv_alloc_pd),
        OBSERVE(ibv_dealloc_pd), OBSERVE(ibv_create_qp), OBSERVE(ibv_destroy_qp),
        OBSERVE(ibv_destroy_cq), OBSERVE(ibv_destroy_comp_channel), OBSERVE(ibv_dereg_mr),
        OBSERVE(ibv_reg_mr), OBSERVE(ibv_reg_mr_iova2)
#undef OBSERVE
    };
    for (size_t i = 0; i < sizeof(observed) / sizeof(observed[0]); ++i) {
        if (strcmp(name, observed[i].name) != 0) continue;
        void *verbs = dlopen("libibverbs.so.1", RTLD_LAZY | RTLD_NOLOAD);
        if (verbs) {
            int matches = library == verbs;
            dlclose(verbs);
            if (matches) return observed[i].wrapper;
        }
        break;
    }
    return native;
}
