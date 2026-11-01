// Multi-representation clipboard source for Mutter. GJS cannot implement the
// read_async vfunc because it has a callback argument. No desktop state is
// changed by this helper until its caller explicitly assigns the source.
//
// Built without Mutter headers or libraries so one binary works with every
// GNOME release: MetaSelectionSource is looked up by name in the running
// Shell, and its public class layout (unchanged since Mutter 3.34) is mirrored
// below. localwispr_selection_create returns NULL instead of registering a
// subtype when the running Mutter's type sizes differ from this mirror.
#include <gio/gio.h>

typedef struct {
    GObject parent_instance;
} MetaSelectionSource;

typedef struct {
    GObjectClass parent_class;
    void (*activated)(MetaSelectionSource *source);
    void (*deactivated)(MetaSelectionSource *source);
    GList *(*get_mimetypes)(MetaSelectionSource *source);
    void (*read_async)(MetaSelectionSource *source, const gchar *mimetype,
                       GCancellable *cancellable, GAsyncReadyCallback callback,
                       gpointer user_data);
    GInputStream *(*read_finish)(MetaSelectionSource *source, GAsyncResult *result,
                                 GError **error);
} MetaSelectionSourceClass;

typedef struct {
    MetaSelectionSource parent;
    GHashTable *contents;
} LocalwisprSelectionSource;

static gpointer parent_class;

static GList *get_mimetypes(MetaSelectionSource *source)
{
    LocalwisprSelectionSource *self = (LocalwisprSelectionSource *) source;
    GHashTableIter iter;
    gpointer key;
    GList *types = NULL;
    g_hash_table_iter_init(&iter, self->contents);
    while (g_hash_table_iter_next(&iter, &key, NULL))
        types = g_list_prepend(types, g_strdup(key));
    return types;
}

static void read_async(MetaSelectionSource *source, const char *mime,
                       GCancellable *cancellable, GAsyncReadyCallback callback,
                       gpointer user_data)
{
    LocalwisprSelectionSource *self = (LocalwisprSelectionSource *) source;
    GBytes *bytes = g_hash_table_lookup(self->contents, mime);
    GTask *task = g_task_new(source, cancellable, callback, user_data);
    if (bytes)
        g_task_return_pointer(task, g_memory_input_stream_new_from_bytes(bytes), g_object_unref);
    else
        g_task_return_new_error(task, G_IO_ERROR, G_IO_ERROR_NOT_SUPPORTED,
                                "Unsupported MIME type: %s", mime);
    g_object_unref(task);
}

static GInputStream *read_finish(MetaSelectionSource *source, GAsyncResult *result,
                                 GError **error)
{
    g_return_val_if_fail(g_task_is_valid(result, source), NULL);
    return g_task_propagate_pointer(G_TASK(result), error);
}

static void finalize(GObject *object)
{
    LocalwisprSelectionSource *self = (LocalwisprSelectionSource *) object;
    g_hash_table_unref(self->contents);
    G_OBJECT_CLASS(parent_class)->finalize(object);
}

static void class_init(gpointer klass, G_GNUC_UNUSED gpointer data)
{
    MetaSelectionSourceClass *source_class = klass;
    parent_class = g_type_class_peek_parent(klass);
    G_OBJECT_CLASS(klass)->finalize = finalize;
    source_class->get_mimetypes = get_mimetypes;
    source_class->read_async = read_async;
    source_class->read_finish = read_finish;
}

static void instance_init(GTypeInstance *instance, G_GNUC_UNUSED gpointer klass)
{
    LocalwisprSelectionSource *self = (LocalwisprSelectionSource *) instance;
    self->contents = g_hash_table_new_full(g_str_hash, g_str_equal, g_free,
                                           (GDestroyNotify) g_bytes_unref);
}

static GType selection_source_type(void)
{
    /* The once-value must be nonzero, while the type may legitimately be
     * G_TYPE_INVALID when the running Mutter is incompatible. */
    static gsize initialized = 0;
    static GType type = G_TYPE_INVALID;
    if (g_once_init_enter(&initialized)) {
        GType parent = g_type_from_name("MetaSelectionSource");
        GTypeQuery query = {0};
        if (parent)
            g_type_query(parent, &query);
        if (parent && g_type_is_a(parent, G_TYPE_OBJECT) &&
            query.class_size == sizeof(MetaSelectionSourceClass) &&
            query.instance_size == sizeof(MetaSelectionSource)) {
            type = g_type_register_static_simple(
                parent, "LocalwisprSelectionSource", sizeof(MetaSelectionSourceClass),
                class_init, sizeof(LocalwisprSelectionSource), instance_init, 0);
        } else {
            g_warning("localwispr: MetaSelectionSource %s; multi-format clipboard restore disabled",
                      parent ? "has an unexpected layout" : "is not registered");
        }
        g_once_init_leave(&initialized, 1);
    }
    return type;
}

/* Returns NULL when the running Mutter is incompatible; the caller then falls
 * back to Meta.SelectionSourceMemory. */
GObject *localwispr_selection_create(GVariant *contents)
{
    g_return_val_if_fail(g_variant_is_of_type(contents, G_VARIANT_TYPE("a{say}")), NULL);
    GType type = selection_source_type();
    if (!type)
        return NULL;
    LocalwisprSelectionSource *self = g_object_new(type, NULL);
    GVariantIter iter;
    char *mime;
    GVariant *value;
    g_variant_iter_init(&iter, contents);
    while (g_variant_iter_next(&iter, "{s@ay}", &mime, &value)) {
        GBytes *bytes = g_variant_get_data_as_bytes(value);
        g_hash_table_replace(self->contents, mime, bytes);
        g_variant_unref(value);
    }
    return G_OBJECT(self);
}
