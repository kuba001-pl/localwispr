// Multi-representation clipboard source for Mutter. GJS cannot implement the
// read_async vfunc because it has a callback argument. No desktop state is
// changed by this helper until its caller explicitly assigns the source.
#include <meta/meta-selection-source.h>

typedef struct {
    MetaSelectionSource parent;
    GHashTable *contents;
} LocalwisprSelectionSource;
typedef MetaSelectionSourceClass LocalwisprSelectionSourceClass;

G_DEFINE_TYPE(LocalwisprSelectionSource, localwispr_selection_source,
              META_TYPE_SELECTION_SOURCE)

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
    G_OBJECT_CLASS(localwispr_selection_source_parent_class)->finalize(object);
}

static void localwispr_selection_source_class_init(LocalwisprSelectionSourceClass *klass)
{
    G_OBJECT_CLASS(klass)->finalize = finalize;
    klass->get_mimetypes = get_mimetypes;
    klass->read_async = read_async;
    klass->read_finish = read_finish;
}

static void localwispr_selection_source_init(LocalwisprSelectionSource *self)
{
    self->contents = g_hash_table_new_full(g_str_hash, g_str_equal, g_free,
                                         (GDestroyNotify) g_bytes_unref);
}

MetaSelectionSource *localwispr_selection_create(GVariant *contents)
{
    g_return_val_if_fail(g_variant_is_of_type(contents, G_VARIANT_TYPE("a{say}")), NULL);
    LocalwisprSelectionSource *self = g_object_new(localwispr_selection_source_get_type(), NULL);
    GVariantIter iter;
    char *mime;
    GVariant *value;
    g_variant_iter_init(&iter, contents);
    while (g_variant_iter_next(&iter, "{s@ay}", &mime, &value)) {
        GBytes *bytes = g_variant_get_data_as_bytes(value);
        g_hash_table_replace(self->contents, mime, bytes);
        g_variant_unref(value);
    }
    return META_SELECTION_SOURCE(self);
}
