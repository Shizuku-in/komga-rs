<template>
  <v-dialog v-model="modal"
            max-width="450"
            :fullscreen="$vuetify.breakpoint.xsOnly"
  >
    <v-card>
      <v-card-title>{{ $t('dialog.edit_recommended.dialog_title') }}</v-card-title>
      <v-btn icon position="absolute" location="top right" @click="dialogClose">
        <v-icon>mdi-close</v-icon>
      </v-btn>

      <v-card-text :class="$vuetify.breakpoint.xsOnly ? 'px-0' : undefined">
        <v-list>
          <draggable
            v-model="localItems"
            item-key="section"
            v-bind="dragOptions"
            handle=".handle"
          >
            <template #item="{ element: l }">
              <v-list-item>
                <template #prepend>
                  <v-icon class="handle">mdi-drag-horizontal-variant</v-icon>
                </template>
                <div>
                  <v-list-item-title class="handle">{{ $t(`dashboard.${l.section.toLowerCase()}`) }}</v-list-item-title>
                </div>
                <template #append>
                  <v-list-item-action>
                    <v-switch v-model="enabled[l.section]"/>
                  </v-list-item-action>
                </template>
              </v-list-item>
            </template>
          </draggable>
        </v-list>
      </v-card-text>

      <v-card-actions>
        <v-spacer/>
        <v-btn v-if="$vuetify.breakpoint.smAndUp" variant="text" @click="dialogClose">{{ $t('common.cancel') }}</v-btn>
        <v-btn color="error" @click="resetToDefault">{{
            $t('dialog.edit_recommended.button_reset')
          }}
        </v-btn>
        <v-btn color="primary" @click="saveChanges">{{
            $t('dialog.edit_recommended.button_confirm')
          }}
        </v-btn>
      </v-card-actions>
    </v-card>
  </v-dialog>
</template>

<script lang="ts">
import {UserRoles} from '@/types/enum-users'
import draggable from 'vuedraggable'
import {defineComponent, PropType} from 'vue'
import {
  ClientSettingsRecommendedView,
  ClientSettingsRecommendedViewSection,
  RECOMMENDED_DEFAULT,
} from '@/types/komga-clientsettings'


export default defineComponent({
  name: 'EditRecommendedDialog',
  components: {draggable},
  data: function () {
    return {
      UserRoles,
      modal: false,
      localItems: [] as ClientSettingsRecommendedViewSection[],
      enabled: {} as Record<string, boolean>,
    }
  },
  props: {
    modelValue: Boolean,
    viewConfig: {
      type: Object as PropType<ClientSettingsRecommendedView>,
      required: true,
    },
  },
  watch: {
    modelValue(val) {
      this.modal = val
      if (val) {
        this.reset(this.viewConfig)
      }
    },
    modal(val) {
      !val && this.dialogClose()
    },
  },
  computed: {
    dragOptions(): any {
      return {
        animation: 200,
        ghostClass: 'ghost',
      }
    },
  },
  methods: {
    reset(viewConfig: ClientSettingsRecommendedView) {
      this.localItems = viewConfig?.sections || []
      this.enabled = []
      this.localItems.forEach(it => this.enabled[it.section] = true)
      RECOMMENDED_DEFAULT.sections
        .filter(it => !viewConfig?.sections.some(s => s.section === it.section))
        .forEach(it => this.localItems.push(it))
    },
    dialogClose() {
      this.$emit('update:modelValue', false)
    },
    resetToDefault() {
      this.$emit('reset-defaults')
      this.dialogClose()
    },
    saveChanges() {
      const sections = this.localItems.filter(it => this.enabled[it.section])
      const updated = {
        sections: sections,
      } as ClientSettingsRecommendedView

      this.$emit('update:viewConfig', updated)
      this.dialogClose()
    },
  },
})
</script>

<style scoped>
.handle {
  cursor: grab !important;
}

.ghost {
  opacity: 0.5;
  background: #c8ebfb;
}
</style>
